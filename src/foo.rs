use crate::error::BenchmarkResult;
use std::fs::File;
use std::io::{self, BufRead};
use std::path::Path;

/// Function to create an iterator over the lines of a file.
pub(crate) fn read_lines<P>(filename: P) -> BenchmarkResult<impl Iterator<Item=io::Result<String>>>
where
    P: AsRef<Path>,
{
    let file = File::open(filename)?; // Open the file
    let reader = io::BufReader::new(file); // Create a buffered reader

    // Return an iterator over the lines
    Ok(reader.lines()) // The lines() method returns an iterator
}
