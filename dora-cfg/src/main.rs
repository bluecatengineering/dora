use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{ArgEnum, Parser};

use config::wire;
use serde::de::DeserializeOwned;

#[derive(Parser, Debug, Clone, PartialEq, Eq)]
#[clap(author, version, about, long_about = None)]
/// Cli tool for parsing config & JSON schema
pub struct Args {
    /// path to dora config. We will determine format from extension. If no extension, we will attempt JSON & YAML
    #[clap(short = 'p', long, value_parser)]
    pub path: PathBuf,
    /// print the parsed wire format or the dora internal config format
    #[clap(short = 'f', long, arg_enum, value_parser)]
    pub format: Option<Format>,
    /// path to JSON schema. Config must be in JSON format and use `.json` extension
    #[clap(short = 's', long, value_parser)]
    pub schema: Option<PathBuf>,
}

#[derive(Parser, Debug, Clone, PartialEq, Eq, ArgEnum)]
pub enum Format {
    Wire,
    Internal,
}

fn main() -> Result<()> {
    let args = Args::parse();
    println!("found config at path = {}", args.path.display());

    parse_schema(&args)?;
    if let Some(format) = &args.format {
        match format {
            Format::Wire => {
                let wire_cfg = parse_wire::<wire::Config>(&args)?;
                println!("printing wire format");
                println!("{:#?}", wire_cfg);
            }
            Format::Internal => {
                let cfg = config::v4::Config::from_wire(parse_wire(&args)?)?;
                println!("parsed wire format into dora internal format, pretty printing");
                println!("{:#?}", cfg);
            }
        }
    }

    Ok(())
}

fn parse_schema(args: &Args) -> Result<()> {
    if let Some(schema) = &args.schema {
        let parsed = serde_json::from_str::<serde_json::Value>(
            &std::fs::read_to_string(schema).context("failed to find schema")?,
        )?;
        let input = parse_wire::<serde_json::Value>(args)?;
        let validator = jsonschema::JSONSchema::options()
            .with_draft(jsonschema::Draft::Draft7)
            .compile(&parsed)
            .expect("failed to compile schema"); // can't use ? static lifetime on error
                                                 // TODO: jsonschema crate has garbage error types!
        return if let Err(errs) = validator.validate(&input) {
            errs.for_each(|err| eprintln!("{}", err));
            Err(anyhow::anyhow!("failed to validate schema"))
        } else {
            println!("json schema validated");
            Ok(())
        };
    }
    Ok(())
}

fn parse_wire<T: DeserializeOwned>(args: &Args) -> Result<T> {
    let input = std::fs::read_to_string(&args.path).context("failed to find config")?;

    Ok(match args.path.extension() {
        Some(ext) if ext == "json" => serde_json::from_str(&input)?,
        Some(ext) if ext == "yaml" => serde_yaml::from_str(&input)?,
        _ => match serde_json::from_str(&input) {
            Ok(r) => r,
            Err(_err) => {
                println!("failed parsing from json, trying yaml");
                serde_yaml::from_str(&input)?
            }
        },
    })
}
