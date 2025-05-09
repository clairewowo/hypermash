use clap::{Arg, ArgAction, Command};
use needletail::{parse_fastx_file, Sequence};
use needletail::kmer::Kmers;
use needletail::sequence::canonical;
use rayon::prelude::*;
use crossbeam_channel::bounded;
use std::error::Error;
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write, BufWriter};
use std::thread;
use std::path::Path;
use hyperminhash::Sketch;

fn main() -> Result<(), Box<dyn Error>> {
    // Initialize logger
    println!("\n ************** initializing logger *****************\n");
    env_logger::Builder::from_default_env().init();
    // Set up the command-line arguments
    let matches = Command::new("Genome Sketching via HyperMinhash")
        .version("0.1.0")
        .about("Fast and Memory Efficient Genome/Metagenome Sketching via HyperMinhash")
        .subcommand(
            Command::new("sketch")
            .about("Creates vectors and serializes them")
            .arg(
                Arg::new("sketch")
                .short('s')
                .long("sketch")
                .help("Two files containing list of FASTA files (.gz supported), separated by space. 
                Query file first, then reference file")
                .required(true)
                .action(ArgAction::Set)
            )
            .arg(
                Arg::new("kmer_length")
                .short('k')
                .long("kmer")
                .help("Length of the kmer")
                .required(true)
                .action(ArgAction::Set)
            )
        )
        .subcommand(
            Command::new("distance")
            .about("Computes distance between sketches")
            .arg(
                Arg::new("distance")
                .short('d')
                .long("help")
                .help("Two files containing serialized sketches")
                .required(true)
                .action(ArgAction::Set)
            )
        )
        // .arg(
        //     Arg::new("query_files")
        //         .short('q')
        //         .long("query_file")
        //         .help("File containing list of query FASTA files, .gz supported")
        //         .required(true)
        //         .action(ArgAction::Set),
        // )
        // .arg(
        //     Arg::new("reference_files")
        //         .short('r')
        //         .long("ref_file")
        //         .help("File containing list of reference FASTA files, .gz supported")
        //         .required(true)
        //         .action(ArgAction::Set),
        // )
        // .arg(
        //     Arg::new("kmer_length")
        //         .short('k')
        //         .long("kmer")
        //         .help("Length of k-mers")
        //         .required(true)
        //         .value_parser(clap::value_parser!(usize))
        //         .action(ArgAction::Set),
        // )
        // .arg(
        //     Arg::new("output_file")
        //         .short('o')
        //         .long("output")
        //         .help("Output file to write results")
        //         .required(true)
        //         .action(ArgAction::Set),
        // )
        .get_matches();
    match matches.subcommand() {
        Some(("sketch", s_matches)) => {
            let sketch_files: Vec<&str> = s_matches.get_one::<String>("sketch").expect("required").split(' ').collect();
            let query_files_list = sketch_files[0];
            let reference_files_list = sketch_files[1];
            let kmer_length: usize = *matches.get_one::<usize>("kmer_length").unwrap();
            
            // Read the lists of query and reference files
            let query_file = File::open(query_files_list)?;
            let query_reader = BufReader::new(query_file);
            let query_files: Vec<String> = query_reader
                .lines()
                .filter_map(|line| line.ok())
                .filter(|line| !line.trim().is_empty())
                .collect();

            let reference_file = File::open(reference_files_list)?;
            let reference_reader = BufReader::new(reference_file);
            let reference_files: Vec<String> = reference_reader
                .lines()
                .filter_map(|line| line.ok())
                .filter(|line| !line.trim().is_empty())
                .collect();

            // Process query files and create sketches
            let query_sketches: HashMap<String, Sketch> = query_files
            .par_iter()
            .map(|file_name| {
                // Clone the file name for ownership in the closure
                let file_name = file_name.clone();

                // Process the file and create a sketch
                let mut reader = parse_fastx_file(&file_name).expect("Failed to parse file");
                let (sender, receiver) = bounded(10); // Channel with capacity 10

                // Spawn a thread to read sequences and send batches
                let reader_thread = thread::spawn(move || {
                    let mut batch = Vec::new();
                    while let Some(result) = reader.next() {
                        if let Ok(seqrec) = result {
                            batch.push(seqrec.seq().to_vec());
                            if batch.len() == 5000 {
                                if sender.send(batch.clone()).is_err() {
                                    break; // Receiver has hung up
                                }
                                batch.clear();
                            }
                        }
                    }
                    if !batch.is_empty() {
                        let _ = sender.send(batch); // Send remaining batch
                    }
                });

                // Initialize an empty Sketch allocated on the heap
                let mut global_sketch = Box::new(Sketch::default());

                // Process the batches
                for batch in receiver {
                    // Process the batch in parallel
                    let local_sketch = batch
                        .par_iter()
                        .map(|seq| {
                            // Allocate sketch on the heap
                            let mut sketch = Box::new(Sketch::default());
                            let kmer_length_u8 = kmer_length as u8;
                            for kmer in Kmers::new(seq, kmer_length_u8) {
                                let kmer_bytes = canonical(kmer); // Use canonical function
                                sketch.add_bytes(&kmer_bytes);
                            }
                            sketch
                        })
                        .reduce(
                            || Box::new(Sketch::default()),
                            |mut a, b| {
                                a.union(&b);
                                a
                            },
                        );

                    // Merge the local sketch into the global sketch
                    global_sketch.union(&local_sketch);
                }

                // Wait for the reader thread to finish
                reader_thread.join().expect("Reader thread panicked");

                (file_name, *global_sketch)
            })
            .collect();

            let query_list = File::create("query_list.bin")?;
            let mut query_list_writer = BufWriter::new(query_list);
            let query_sketch = File::create("query_sketches.bin")?;
            let mut query_sketch_writer = BufWriter::new(query_sketch);

            // serialize query sketches
            for (string, sketch) in query_sketches {
                writeln!(query_list_writer, "{}", string)?;
                sketch.save(&mut query_sketch_writer)?;
            }
            println!("Serialized query sketches");

            let reference_sketches: HashMap<String, Sketch> = reference_files
            .par_iter()
            .map(|file_name| {
                // Clone the file name for ownership in the closure
                let file_name = file_name.clone();

                // Process the file and create a sketch
                let mut reader = parse_fastx_file(&file_name).expect("Failed to parse file");
                let (sender, receiver) = bounded(10); // Channel with capacity 10

                // Spawn a thread to read sequences and send batches
                let reader_thread = thread::spawn(move || {
                    let mut batch = Vec::new();
                    while let Some(result) = reader.next() {
                        if let Ok(seqrec) = result {
                            batch.push(seqrec.seq().to_vec());
                            if batch.len() == 5000 {
                                if sender.send(batch.clone()).is_err() {
                                    break; // Receiver has hung up
                                }
                                batch.clear();
                            }
                        }
                    }
                    if !batch.is_empty() {
                        let _ = sender.send(batch); // Send remaining batch
                    }
                });

                // Initialize an empty Sketch allocated on the heap
                let mut global_sketch = Box::new(Sketch::default());

                // Process the batches
                for batch in receiver {
                    // Process the batch in parallel
                    let local_sketch = batch
                        .par_iter()
                        .map(|seq| {
                            // Allocate sketch on the heap
                            let mut sketch = Box::new(Sketch::default());
                            let kmer_length_u8 = kmer_length as u8;
                            for kmer in Kmers::new(seq, kmer_length_u8) {
                                let kmer_bytes = canonical(kmer); // Use canonical function
                                sketch.add_bytes(&kmer_bytes);
                            }
                            sketch
                        })
                        .reduce(
                            || Box::new(Sketch::default()),
                            |mut a, b| {
                                a.union(&b);
                                a
                            },
                        );

                    // Merge the local sketch into the global sketch
                    global_sketch.union(&local_sketch);
                }

                // Wait for the reader thread to finish
                reader_thread.join().expect("Reader thread panicked");

                (file_name, *global_sketch)
            })
            .collect();

            let ref_list = File::create("reference_list.bin")?;
            let mut ref_list_writer = BufWriter::new(ref_list);
            let ref_sketch = File::create("reference_sketches.bin")?;
            let mut ref_sketch_writer = BufWriter::new(ref_sketch);

            // serialize query sketches
            for (string, sketch) in reference_sketches {
                writeln!(ref_list_writer, "{}", string)?;
                sketch.save(&mut ref_sketch_writer)?;
            }
            println!("Serialized reference sketches");
            Ok(())
        }
        Some(("distance", s_matches)) => {
            let distance_files:Vec<&str> = s_matches.get_one::<String>("distance").expect("required").split(' ').collect();
            let sketch_file = File::open(distance_files[0]).expect("Failed to create file");
            let ref_file = File::open(distance_files[1]).expect("Failed to create file");

            // write code to read in sketch and ref files, put it into a hashmap. 
            let mut sketch_reader = BufReader::new(sketch_file);
            let mut str_reader = BufReader::new(ref_file);

            // Generate all pairs of query and reference sketches
            // let pairs: Vec<(&String, &Sketch, &String, &Sketch)> = query_sketches
            // .iter()
            // .flat_map(|(q_name, q_sketch)| {
            //     reference_sketches.iter().map(move |(r_name, r_sketch)| {
            //         (q_name, q_sketch, r_name, r_sketch)
            //     })
            // })
            // .collect();

            // // Compute similarities and distances in parallel
            // let results: Vec<(String, String, f64)> = pairs
            //     .par_iter()
            //     .map(|&(query_name, query_sketch, reference_name, reference_sketch)| {
            //         let similarity = query_sketch.similarity(reference_sketch);

            //         // Avoid division by zero and log of zero
            //         let adjusted_similarity = if similarity <= 0.0 {
            //             std::f64::EPSILON // Small positive number to avoid log(0)
            //         } else {
            //             similarity
            //         };

            //         // Calculate distance using the provided formula
            //         let numerator = 2.0 * adjusted_similarity;
            //         let denominator = 1.0 + adjusted_similarity;
            //         let fraction = numerator / denominator;
            //         let distance = -fraction.ln() / (kmer_length as f64);

            //         (query_name.clone(), reference_name.clone(), distance)
            //     })
            //     .collect();

            // // Open the output file for writing
            // let mut output = File::create(output_file)?;

            // // Write header line
            // writeln!(output, "Query\tReference\tDistance")?;

            // // Write the results with file path normalization
            // for (query_name, reference_name, distance) in &results {
            //     // Extract file names from the paths
            //     let query_basename = Path::new(&query_name)
            //         .file_name()
            //         .and_then(|os_str| os_str.to_str())
            //         .unwrap_or(&query_name);

            //     let reference_basename = Path::new(&reference_name)
            //         .file_name()
            //         .and_then(|os_str| os_str.to_str())
            //         .unwrap_or(&reference_name);

            //     let distance = if query_basename == reference_basename {
            //         0.0
            //     } else {
            //         *distance
            //     };
            //     writeln!(output, "{}\t{}\t{:.6}", query_name, reference_name, distance)?;
            // }

            Ok(())
        }
        _ => {
            Ok(())
        }
    }
    
}
