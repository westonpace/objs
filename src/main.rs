use clap::{Parser, Subcommand};
use futures::TryStreamExt;
use object_store::{path::Path, GetResultPayload, ObjectStore};
use path_clean::PathClean;
use shellexpand::tilde;
use thiserror::Error;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use url::{form_urlencoded::Parse, Url};

const BUF_SIZE: usize = 1024 * 1024 * 8;

#[derive(Error, Debug)]
pub enum ObjsError {
    #[error("The provided path `{0}` does not appear to be a valid URI or path: {1}")]
    InvalidUriOrPath(String, String),
    #[error("I/O error on path `{0}`: {1}")]
    StdIoError(String, std::io::Error),
    #[error("Library error: {0}")]
    ObjectStoreError(#[from] object_store::Error),
    #[error("Invalid path: {0}")]
    ObjectStorePathError(#[from] object_store::path::Error),
}

type Result<T> = std::result::Result<T, ObjsError>;

struct ParsedUriOrPath {
    path: Path,
    store: Box<dyn ObjectStore>,
}

fn object_store_from_path(path: &str) -> Result<ParsedUriOrPath> {
    let expanded = tilde(path).to_string();
    let expanded_path = std::path::Path::new(&expanded);

    let expanded_path = expanded_path.clean();

    let expanded_path = expanded_path.as_os_str().to_str().ok_or_else(|| {
        ObjsError::InvalidUriOrPath(
            path.to_string(),
            "the path does not appear to be valid UTF-8".to_string(),
        )
    })?;

    let parsed_path = Path::parse(expanded_path)?;

    Ok(ParsedUriOrPath {
        path: parsed_path,
        store: Box::new(object_store::local::LocalFileSystem::new()),
    })
}

fn object_store_from_uri(uri: Url) -> Result<ParsedUriOrPath> {
    match uri.scheme() {
        _ => {
            dbg!("Parsing URL");
            let (store, path) = object_store::parse_url_opts(&uri, [("region", "us-east-1")])?;
            dbg!("Parsed");
            Ok(ParsedUriOrPath { path, store })
        }
    }
}

fn object_store_from_uri_or_path(uri_or_path: &str) -> Result<ParsedUriOrPath> {
    match Url::parse(uri_or_path) {
        Ok(url) if url.scheme().len() == 1 && cfg!(windows) => {
            // On Windows, the drive is parsed as a scheme
            object_store_from_path(uri_or_path)
        }
        Ok(url) => object_store_from_uri(url),
        Err(_) => object_store_from_path(uri_or_path),
    }
}

#[derive(Parser, Debug)]
struct CopyArgs {
    #[arg(value_name = "SRC: PATH_OR_URI")]
    source: String,
    #[arg(value_name = "DST: PATH_OR_URI")]
    dest: String,
}

#[derive(Debug, Subcommand)]
enum SubCmd {
    Cp(CopyArgs),
}

/// Simple program to greet a person
#[derive(Parser, Debug)]
#[command(author, version, about, long_about = None)]
struct Args {
    #[command(subcommand)]
    subcommand: SubCmd,
}

async fn handle_copy(_global_args: &Args, copy_args: &CopyArgs) -> Result<()> {
    let src = object_store_from_uri_or_path(&copy_args.source)?;
    let dst = object_store_from_uri_or_path(&copy_args.dest)?;

    dbg!(format!("Starting get: {}", src.path));
    let src_file = src.store.get(&src.path).await?;
    dbg!("Done getting src: {src_file:?}");
    let (_dst_id, mut dst_write) = dst.store.put_multipart(&dst.path).await?;

    match src_file.payload {
        GetResultPayload::File(file, _path) => {
            let mut file = tokio::fs::File::from_std(file);
            let mut buffer = vec![0; BUF_SIZE];
            loop {
                let bytes_read = file
                    .read(&mut buffer)
                    .await
                    .map_err(|io_err| ObjsError::StdIoError(src.path.to_string(), io_err))?;
                if bytes_read == 0 {
                    break;
                }
                dst_write
                    .write_all(&buffer)
                    .await
                    .map_err(|io_err| ObjsError::StdIoError(dst.path.to_string(), io_err))?;
            }
        }
        GetResultPayload::Stream(mut stream) => {
            dbg!("Streaming!");
            while let Some(bytes) = stream.try_next().await? {
                dbg!(bytes.len());
                dst_write
                    .write_all(&bytes)
                    .await
                    .map_err(|io_err| ObjsError::StdIoError(dst.path.to_string(), io_err))?;
            }
        }
    }
    dst_write
        .shutdown()
        .await
        .map_err(|io_err| ObjsError::StdIoError(dst.path.to_string(), io_err))?;

    Ok(())
}

async fn handle_command(args: Args) -> Result<()> {
    match &args.subcommand {
        SubCmd::Cp(copy_args) => handle_copy(&args, &copy_args).await,
    }
}

#[tokio::main]
async fn main() {
    let args = Args::parse();
    let res = handle_command(args).await;
    if let Err(err) = res {
        println!("{err}");
    }
}
