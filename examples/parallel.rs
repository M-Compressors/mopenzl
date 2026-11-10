//! Caller-selected executors and a reusable parallel compressor.
//! Run with `--features hotpath-cpu,hotpath-alloc` for a profiling report.

#[cfg(not(feature = "no_std"))]
#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() -> Result<(), Box<dyn std::error::Error>> {
    use mopenzl::compressor::parallel::{
        BatchConfig, ParallelCompressor, ParallelConfig, TaskCount,
    };
    use mopenzl::{Decompressor, EncoderConfig};
    let input = b"parallel OpenZL chunks with caller-owned executors\n".repeat(200_000);
    let mut compressor =
        ParallelCompressor::new(EncoderConfig::default(), ParallelConfig::default())?;
    let mut output = Vec::new();
    for _ in 0..5 {
        let mut batch =
            compressor.prepare_slice(&input, BatchConfig::auto(TaskCount::try_from(4)?))?;
        let tasks = batch.take_tasks()?;
        std::thread::scope(|scope| {
            for task in tasks {
                scope.spawn(move || task.run());
            }
        });
        output.clear();
        batch.finish_into(&mut output)?;
        assert_eq!(Decompressor::default().decompress(&output)?, input);
    }
    println!("{} -> {} bytes", input.len(), output.len());
    Ok(())
}
#[cfg(feature = "no_std")]
fn main() {}
