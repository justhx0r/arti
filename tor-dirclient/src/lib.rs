//! Implements a directory client for Tor.
//!
//! Tor makes directory requests as HTTP/1.0 requests tunneled over Tor circuits.
//! For most objects, Tor uses a one-hop tunnel.
//!
//! # Limitations
//!
//! Multi-hop tunnels are not supported.

// XXXX THIS CODE IS HORRIBLE AND NEEDS REFACTORING.

#![deny(missing_docs)]
#![deny(clippy::missing_docs_in_private_items)]

mod err;
pub mod request;
mod response;
mod util;

use tor_circmgr::{CircMgr, DirInfo};
use tor_decompress::{Decompressor, StatusKind};

use anyhow::{Context, Result};
use futures::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use futures::FutureExt;
use log::info;
use std::sync::Arc;
use std::time::Duration;

pub use err::Error;
pub use response::{DirResponse, SourceInfo};

/// Fetch the resource described by `req` over the Tor network.
///
/// Circuits are built or found using `circ_mgr`, using paths
/// constructed using `dirinfo`.
pub async fn get_resource<CR>(
    req: CR,
    dirinfo: DirInfo<'_>,
    circ_mgr: Arc<CircMgr>,
) -> Result<DirResponse>
where
    CR: request::ClientRequest,
{
    use tor_rtcompat::timer::timeout;

    let circuit = circ_mgr.get_or_launch_dir(dirinfo).await?;

    // XXXX should be an option, and is too long.
    let begin_timeout = Duration::from_secs(5);
    let source = SourceInfo::new(circuit.unique_id());
    let mut stream = timeout(begin_timeout, async {
        // Send the HTTP request
        circuit
            .begin_dir_stream()
            .await
            .with_context(|| format!("Failed to open a directory stream to {:?}", source))
    })
    .await??;

    // TODO: Perhaps we want separate timeouts for each phase of this.
    // For now, we just use higher-level timeouts in `dirmgr`.
    let r = download(req, &mut stream, Some(source.clone())).await;

    // If this is a "persistent" error, then we shouldn't use this circuit
    // any more.
    if let Err(ref e) = r {
        if false {
            // e.is_fatal() { XXXX
            retire_circ(circ_mgr, &source, &e).await;
        }
    }

    r
}

/// Fetch a Tor directory object from a provided stream.
///
/// To do this, we send a simple HTTP/1.0 request for the described
/// object in `req` over `stream`, and then wait for a response.  In
/// log messatges, we describe the origin of the data as coming from
/// `source`.
///
/// # Notes
///
/// This code does no timeouts.
///
/// It's kind of bogus to have a 'source' field here at all.
pub async fn download<R, S>(
    req: R,
    stream: &mut S,
    source: Option<SourceInfo>,
) -> Result<DirResponse>
where
    R: request::ClientRequest,
    S: AsyncRead + AsyncWrite + Unpin,
{
    let partial_ok = req.partial_docs_ok();
    let maxlen = req.max_response_len();
    let req = req.into_request()?;
    let encoded = util::encode_request(req);

    // Write the request.
    stream
        .write_all(encoded.as_bytes())
        .await
        .with_context(|| format!("Failed to send HTTP request to {:?}", source))?;
    stream
        .flush()
        .await
        .with_context(|| format!("Couldn't deliver http request to {:?}", source))?;

    // Handle the response
    let header = read_headers(stream)
        .await
        .with_context(|| format!("Failed to handle the HTTP response from {:?}", source))?;

    if header.status != Some(200) {
        return Err(Error::HttpStatus(header.status).into());
    }

    let encoding = header.encoding;
    let buf = header.pending;
    let n_in_buf = header.n_pending;

    let decompressor = tor_decompress::from_content_encoding(encoding.as_deref())?;

    let mut result = vec![0_u8; 2048];

    let ok = read_and_decompress(stream, maxlen, decompressor, buf, n_in_buf, &mut result).await;
    match (partial_ok, ok, result.len()) {
        (true, Err(_), n) if n > 0 => {
            // retire_circ(Arc::clone(&circ_mgr), &source, &e).await; //XXXX
            // Note that we _don't_ return here: we want the partial response.
        }
        (_, Err(e), _) => {
            return Err(e);
        }
        (_, _, _) => (),
    }

    match String::from_utf8(result) {
        Err(e) => Err(e.into()),
        Ok(output) => Ok(DirResponse::new(200, output, source)),
    }
}

/// Read and parse HTTP/1 headers from `stream`.
async fn read_headers<S>(stream: &mut S) -> Result<HeaderStatus>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut buf = vec![0; 1024];
    let mut n_in_buf = 0;

    loop {
        let n = stream.read(&mut buf[n_in_buf..]).await?;
        n_in_buf += n;

        // XXXX Better maximum and/or let this expand.
        let mut headers = [httparse::EMPTY_HEADER; 32];
        let mut response = httparse::Response::new(&mut headers);

        match response.parse(&buf[..n_in_buf])? {
            httparse::Status::Partial => {
                // We didn't get a whole response; we may need to try again.

                // XXXX Pick a better maximum
                if n_in_buf >= buf.len() - 500 {
                    // We should resize the buffer; it's nearly empty.
                    if buf.len() >= 16384 {
                        return Err(httparse::Error::TooManyHeaders.into());
                    }
                    buf.resize(buf.len() * 2, 0u8);
                }
            }
            httparse::Status::Complete(n_parsed) => {
                if response.code != Some(200) {
                    return Ok(HeaderStatus {
                        status: response.code,
                        encoding: None,
                        pending: Vec::new(),
                        n_pending: 0,
                    });
                }
                let encoding = if let Some(enc) = response
                    .headers
                    .iter()
                    .find(|h| h.name == "Content-Encoding")
                {
                    Some(String::from_utf8(enc.value.to_vec())?)
                } else {
                    None
                };
                /*
                if let Some(clen) = response.headers.iter().find(|h| h.name == "Content-Length") {
                    let clen = std::str::from_utf8(clen.value)?;
                    length = Some(clen.parse()?);
                }
                 */
                n_in_buf -= n_parsed;
                buf.copy_within(n_parsed.., 0);
                return Ok(HeaderStatus {
                    status: Some(200),
                    encoding,
                    pending: buf,
                    n_pending: n_in_buf,
                });
            }
        }
        if n == 0 {
            return Err(Error::TruncatedHeaders.into());
        }
    }
}

/// Return value from read_headers
struct HeaderStatus {
    /// HTTP status code.
    status: Option<u16>,
    /// The Content-Encoding header, if any.
    encoding: Option<String>,
    /// A buffer containing leftover data beyond what was in the header.
    pending: Vec<u8>,
    /// The number of usable bytes in `pending`.
    n_pending: usize,
}

/// Helper: download directory information from `stream` and
/// decompress it into a result buffer.  Assumes we've started with
/// n_in_buf bytes of partially downloaded data in `buf`.
///
/// If we get more than maxlen bytes after decompression, give an error.
///
/// Returns the status of our download attempt, stores any data that
/// we were able to download into `result`.  Existing contents of
/// `result` are overwritten.
async fn read_and_decompress<S>(
    mut stream: S,
    maxlen: usize,
    mut decompressor: Box<dyn Decompressor + Send>,
    mut buf: Vec<u8>,
    mut n_in_buf: usize,
    result: &mut Vec<u8>,
) -> Result<()>
where
    S: AsyncRead + AsyncWrite + Unpin,
{
    let mut read_total = n_in_buf;
    let mut written_total = 0;

    let mut done_reading = false;

    // XXX should be an option and is too long.
    let read_timeout = Duration::from_secs(10);
    let timer = tor_rtcompat::timer::sleep(read_timeout).fuse();
    futures::pin_mut!(timer);

    loop {
        let status = futures::select! {
            status = stream.read(&mut buf[n_in_buf..]).fuse() => status,
            _ = timer => {
                result.resize(written_total, 0);
                return Err(Error::DirTimeout.into());
            }
        };
        let n = match status {
            Ok(n) => n,
            Err(other) => {
                result.resize(written_total, 0);
                return Err(other.into());
            }
        };
        if n == 0 {
            done_reading = true;
        }
        read_total += n;
        n_in_buf += n;

        if result.len() == written_total {
            result.resize(result.len() * 2, 0);
        }

        let st = decompressor.process(&buf[..n_in_buf], &mut result[written_total..], done_reading);
        let st = match st {
            Ok(st) => st,
            Err(e) => {
                result.resize(written_total, 0);
                return Err(e);
            }
        };
        n_in_buf -= st.consumed;
        buf.copy_within(st.consumed.., 0);
        written_total += st.written;

        if written_total > 2048 && written_total > read_total * 20 {
            result.resize(written_total, 0);
            return Err(Error::CompressionBomb.into());
        }
        if written_total > maxlen {
            result.resize(maxlen, 0);
            return Err(Error::ResponseTooLong(written_total).into());
        }

        match st.status {
            StatusKind::Done => break,
            StatusKind::Written => (),
            StatusKind::OutOfSpace => result.resize(result.len() * 2, 0),
        }
    }
    result.resize(written_total, 0);

    Ok(())
}

/// Retire a directory circuit because of an error we've encountered on it.
async fn retire_circ<E>(circ_mgr: Arc<CircMgr>, source_info: &SourceInfo, error: &E)
where
    E: std::fmt::Display,
{
    let id = source_info.unique_circ_id();
    info!(
        "{}: Retiring circuit because of directory failure: {}",
        &id, &error
    );
    circ_mgr.retire_circ(&id).await;
}
