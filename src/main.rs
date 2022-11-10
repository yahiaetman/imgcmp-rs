use std::{process::ExitCode, io::Write};

use clap::{ArgAction, ArgMatches, arg, value_parser, command};
use image;
use anyhow::{self, Context};
use itertools::Itertools;

// This enum defines a threshold either as:
// - An absolute integer value (e.g. the number of pixels in the image)
// - A ratio value (e.g. the percentage of pixels in the image)
#[derive(Clone, Copy)]
enum Threshold {
    Absolute(u32),
    Ratio(f32),
}

impl Threshold {
    // Given the image size, return the threshold value in number of pixels. 
    fn get_actual_threshold(&self, image_size: (u32, u32)) -> u32 {
        match self {
            Threshold::Absolute(value) => *value,
            Threshold::Ratio(ratio) => (ratio * (image_size.0 * image_size.1) as f32) as u32,
        }
    }
}

impl TryFrom<&str> for Threshold {
    type Error = anyhow::Error;
    // Try to parse a string into a threshold.
    // If the string ends with "%", it is a ratio. Otherwise, it is an absolute value.
    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if value.ends_with("%") {
            Ok(Threshold::Ratio(value[0..value.len()-1].parse::<f32>()? / 100f32))
        } else {
            Ok(Threshold::Absolute(value.parse::<u32>()?))
        }
    }
}

// A type used to specify the level of verbosity (higher value -> more verbose).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Verbosity(i32);

impl Verbosity {
    pub const SILENT: Verbosity = Verbosity(0);     // Nothing should be printed.
    pub const DEFAULT: Verbosity = Verbosity(1);
    pub const VERBOSE: Verbosity = Verbosity(2);    // Print extra information.
}

// The comparison options
struct Options {
    value_threshold: u8,        // A threshold [0-1] on the maximum allowed per-channel error.
                                // if 0, any difference passes the threshold. if 1, nothing passes the threshold.
    error_threshold: Threshold, // The number (or percentage) of pixels allowed to be different before the result is considered a mismatch.
    output: Option<String>,     // The path to the pixel error image.
    verbosity: Verbosity,       // The level of verbosity of the comparison operation.
}

impl TryFrom<&ArgMatches> for Options {
    type Error = anyhow::Error;
    // Try to extract the comparison options from the arguments
    fn try_from(args: &ArgMatches) -> Result<Self, Self::Error> {

        let value_threshold = (args.get_one::<f32>("threshold").unwrap_or(&0.0f32) * 255f32) as u8;
        
        let error_threshold = args.get_one::<Threshold>("error").ok_or(anyhow::Error::msg("Failed to parse error threshold"))?.clone();

        let output = args.get_one::<String>("output").map(|s| s.clone());

        let verbosity = 
            if args.get_flag("silent") { Verbosity::SILENT }
            else if args.get_flag("verbose") { Verbosity::VERBOSE }
            else { Verbosity::DEFAULT };
        
        Ok(Options {
            value_threshold,
            error_threshold,
            output,
            verbosity,
        })
    }
}

// Run the comparison command for the given image paths, using the given options.
// Return true if the images match and false otherwise.
fn run(image_paths: [&String; 2], options: &Options) -> anyhow::Result<bool> {
    // Read the two images and convert them to RGB (u8) Images.
    let (img1, img2) = image_paths.iter()
    .map(|&img_path| -> anyhow::Result<image::RgbImage> {
        let reader = image::io::Reader::open(img_path).context(format!("Failed to read {}", img_path))?;
        let image = reader.decode().context(format!("Failed to decode {}", img_path))?;
        
        Ok(image.to_rgb8())
    }).collect_tuple().unwrap();
    let (img1, img2) = (img1?, img2?);

    // Get the image size and check that both images has the same size.
    let size = {
        let (size1, size2) = (img1.dimensions(), img2.dimensions());
        if size1 != size2 {
            if options.verbosity > Verbosity::SILENT {
                println!("Images have different sizes (Got ({}x{}) and ({}x{})).", size1.0, size1.1, size2.0, size2.1);
            }
            return Ok(false);
        }
        size1
    };

    let value_threshold = options.value_threshold;
    let error_thresold = options.error_threshold.get_actual_threshold(size);    
    
    // Allocate an image to store the error between the two images
    let mut error_img = image::RgbImage::new(size.0, size.1);
    
    let mut wrong_pixels: u32 = 0; // The number of pixels that differ by more than the value threshold

    // Loop over all the pixels, compute the difference and populate the  error image
    for x in 0..size.0 {
        for y in 0..size.1 {

            let (pixel1, pixel2) = (img1.get_pixel(x, y), img2.get_pixel(x, y));
            
            let mut is_pixel_different = false;
            // For each pair of channels, compute the absolute difference and check it exceeds the value threshold
            let diff: (u8, u8, u8) = pixel1.0.iter().zip(pixel2.0.iter()).map(|(v1, v2)| {
                let diff = v1.abs_diff(*v2);
                if diff > value_threshold {
                    is_pixel_different = true; // A pair of pixels are mismatched if their difference exceed the threshold in any channel.
                    return 128 | diff >> 1; // To make sure that any wrong pixel is visible in the error image, we remap the error from [0-255] to [128-255].
                } else {
                    return 0; // If the difference if below the threshold, we snap it to 0.
                }
            }).collect_tuple().unwrap();
            
            error_img.get_pixel_mut(x, y).0 = [diff.0, diff.1, diff.2];
            
            if is_pixel_different { wrong_pixels += 1; }
        }
    }

    // If an outut image path was given, save the error image to it.
    if let Some(output_path) = &options.output {
        error_img.save(output_path)?;
    }

    // The images are considered different if the number of wrong pixels exceed the error threshold
    let mismatch  = wrong_pixels > error_thresold;
    
    // Prints the results according to the given verbosity level
    if options.verbosity > Verbosity::SILENT {
        println!("{}", if mismatch {"MISMATCH DETECTED"} else {"MATCH"});
        if options.verbosity == Verbosity::VERBOSE {
            println!("Different Pixels: {}%", (100 * wrong_pixels) as f32 / (size.0 * size.1) as f32);
        }
    }

    Ok(!mismatch)
}

fn main() -> anyhow::Result<ExitCode> {
    
    // Parse the commandline arguments
    
    let args = command!()
        .long_about(
"imgcmp: a simple pixel-wise image comparator\n
    This tool compares between two images pixel by pixel.\n
    For each pixel, the channels are compared with their counterparts.\n
    If the value error for any channel exceeds the threshold, the whole pixel is considered different.\n
    If the number of different pixels exceeds the specified limit, the result is a mismatch.\n
    The exit code will be 0 if the images match and -1 if they don't.\n
    When generating an error image, channels that don't pass the threshold will be kept 0.\n
    Otherwise the channel's value will be 128 (half intensity) plus half the error value.\n"
        )
        .arg(arg!([first_image_path] "The path to the first image in the comparison").required(true))
        .arg(arg!([second_image_path] "The path to the second image in the comparison").required(true))
        .arg(arg!(-t --threshold <THRESHOLD> "Sets a threshold [0-1] on the maximum allowed per-channel error. if 0, any difference passes the threshold. if 1, nothing passes the threshold.")
            .value_parser(value_parser!(f32)).default_value("0"))
        .arg(arg!(-e --error <ERROR> "Sets the number of pixels allowed to be different before the result is considered a mismatch.")
            .value_parser(|s: &str| Threshold::try_from(s)).default_value("0"))
        .arg(arg!(-o --output <OUTPUT> "Outputs the pixel error into an image at the given path."))
        .arg(arg!(-s --silent ... "Run in silent mode. No console output will be generated.").action(ArgAction::SetTrue))
        .arg(arg!(-v --verbose ... "Run in verbose mode. Extra console output will be generated.").action(ArgAction::SetTrue))
        .get_matches();
    
    // Get the image paths and options from the arguments.
    
    let image_paths: Vec<&String> = 
        ["first_image_path", "second_image_path"].iter()
        .map(|&name| -> anyhow::Result<&String> {
            args.get_one::<String>(name).ok_or(anyhow::Error::msg(format!("{} is missing", name)))
        }).collect::<anyhow::Result<Vec<&String>>>()?;

    let options = Options::try_from(&args)?;

    // Run the comparison and specify an exit code based on the result.
    // If there was an error durng the comparison, we only print it if the silent flag was not set.

    match run([image_paths[0], image_paths[1]], &options) {
        Ok(same) => {
            Ok(if same { ExitCode::SUCCESS } else { ExitCode::FAILURE })
        },
        Err(err) => {
            if options.verbosity > Verbosity::SILENT {
                writeln!(std::io::stderr(), "Error {err:?}").expect("Failed to write Error");
            }
            Ok(ExitCode::FAILURE)
        },
    }
}