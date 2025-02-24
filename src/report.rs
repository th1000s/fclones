//! Output formatting.

use std::borrow::Borrow;
use std::cell::Cell;
use std::cmp::min;
use std::fmt::Display;
use std::io;
use std::io::{BufRead, BufReader, Error, ErrorKind, Read, Write};

use chrono::{DateTime, FixedOffset};
use console::style;
use fallible_iterator::FallibleIterator;
use itertools::Itertools;
use lazy_static::lazy_static;
use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::config::OutputFormat;
use crate::files::{FileHash, FileLen};
use crate::path::Path;
use crate::util::IteratorWrapper;
use crate::{FileGroup, TIMESTAMP_FMT};

/// Describes how many redundant files were found, in how many groups,
/// how much space can be reclaimed, etc.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FileStats {
    pub group_count: usize,
    pub redundant_file_count: usize,
    pub redundant_file_size: FileLen,
}

/// Data in the header of the whole report.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ReportHeader {
    /// The program version that produced the report
    pub version: String,
    /// The date and time when the report was produced
    pub timestamp: DateTime<FixedOffset>,
    /// Full shell command containing arguments of the search run that produced the report
    pub command: Vec<String>,
    /// Information on the number of duplicate files reported.
    /// This is optional to allow streaming the report out before finding all files in the future.
    pub stats: Option<FileStats>,
}

/// A helper struct that allows to serialize the report with serde.
/// Together with `IteratorWrapper` used as `groups` it allows to serialize
/// a report in a streaming way, without the need to keep all groups in memory at once.
#[derive(Serialize)]
struct SerializableReport<'a, G: Serialize> {
    header: &'a ReportHeader,
    groups: G,
}

/// A structure for holding contents of the report after fully deserializing the report.
/// Used only by report readers that deserialize the whole report at once.
/// Paths are represented as strings, because strings are more memory efficient than Path here,
/// because we can't do prefix compression that `Path` was designed for.
#[derive(Deserialize)]
struct DeserializedReport {
    header: ReportHeader,
    groups: Vec<FileGroup<String>>,
}

/// Formats and writes duplicate files report to a stream.
/// Supports many formats: text, csv, json, etc.
pub struct ReportWriter<W: Write> {
    out: W,
    color: bool,
}

impl<W: Write> ReportWriter<W> {
    pub fn new(out: W, color: bool) -> ReportWriter<W> {
        ReportWriter { out, color }
    }

    fn write_header_line(&mut self, line: &str) -> io::Result<()> {
        writeln!(
            self.out,
            "{}",
            style(format!("# {}", line))
                .cyan()
                .force_styling(self.color)
        )
    }

    /// Writes the report in human-readable text format.
    ///
    /// A group of identical files starts with a group header at column 0,
    /// containing the size and hash of each file in the group.
    /// Then file paths are printed in separate, indented lines.
    ///
    /// # Example
    /// ```text
    /// # Report by fclones 0.12.0
    /// # Timestamp: Mon, 03 May 2021 13:22:51 +0000
    /// # Command: target/debug/fclones find . -o report.txt
    /// # Found 553 file groups
    /// # 271.8 MB in 4266 redundant files can be removed
    /// 5649a555c131508c4a757d9e14c4aea6, 6626689 B (6.6 MB) * 5:
    ///     /home/pkolaczk/Projekty/fclones/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.0/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.1/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.2/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.11.0/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta
    /// f79ce189d76620fd921986943087dc3a, 5815999 B (5.8 MB) * 5:
    ///     /home/pkolaczk/Projekty/fclones/target/debug/deps/libserde-af05e0212e5def7d.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.0/target/debug/deps/libserde-af05e0212e5def7d.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.1/target/debug/deps/libserde-af05e0212e5def7d.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.2/target/debug/deps/libserde-af05e0212e5def7d.rmeta
    ///     /home/pkolaczk/Projekty/fclones/target/package/fclones-0.11.0/target/debug/deps/libserde-af05e0212e5def7d.rmeta
    /// ```
    pub fn write_as_text<I, G, P>(&mut self, header: &ReportHeader, groups: I) -> io::Result<()>
    where
        I: IntoIterator<Item = G>,
        G: Borrow<FileGroup<P>>,
        P: Display,
    {
        let command = shell_words::join(header.command.iter());
        self.write_header_line(&format!("Report by fclones {}", header.version))?;
        self.write_header_line(&format!(
            "Timestamp: {}",
            header.timestamp.format(TIMESTAMP_FMT)
        ))?;
        self.write_header_line(&format!("Command: {}", command))?;
        if let Some(stats) = &header.stats {
            self.write_header_line(&format!("Found {} file groups", stats.group_count))?;
            self.write_header_line(&format!(
                "{} B ({}) in {} redundant files can be removed",
                stats.redundant_file_size.0, stats.redundant_file_size, stats.redundant_file_count
            ))?;
        }

        for g in groups {
            let g = g.borrow();
            let group_header = format!(
                "{}, {} B ({}) * {}:",
                g.file_hash,
                g.file_len.0,
                g.file_len,
                g.files.len()
            );
            let group_header = style(group_header).yellow();
            writeln!(self.out, "{}", group_header.force_styling(self.color),)?;
            for f in g.files.iter() {
                writeln!(self.out, "    {}", f)?;
            }
        }
        Ok(())
    }

    /// Writes the report in `fdupes` compatible format.
    /// This is very similar to the TEXT format, but there are no headers
    /// for each group, and groups are separated with empty lines.
    pub fn write_as_fdupes<I, G, P>(&mut self, _header: &ReportHeader, groups: I) -> io::Result<()>
    where
        I: IntoIterator<Item = G>,
        G: Borrow<FileGroup<P>>,
        P: Display,
    {
        for g in groups {
            let g = g.borrow();
            for f in g.files.iter() {
                writeln!(self.out, "{}", f)?;
            }
            writeln!(self.out)?;
        }
        Ok(())
    }

    /// Writes results in CSV format.
    ///
    /// Each file group is written as one line.
    /// The number of columns is dynamic.
    /// Columns:
    /// - file size in bytes
    /// - file hash (may be empty)
    /// - number of files in the group
    /// - file paths - each file in a separate column
    pub fn write_as_csv<I, G, P>(&mut self, _header: &ReportHeader, groups: I) -> io::Result<()>
    where
        I: IntoIterator<Item = G>,
        G: Borrow<FileGroup<P>>,
        P: Display,
    {
        let mut wtr = csv::WriterBuilder::new()
            .delimiter(b',')
            .quote_style(csv::QuoteStyle::Necessary)
            .flexible(true)
            .from_writer(&mut self.out);

        wtr.write_record(&["size", "hash", "count", "files"])?;
        for g in groups {
            let g = g.borrow();
            let mut record = csv::StringRecord::new();
            record.push_field(g.file_len.0.to_string().as_str());
            record.push_field(g.file_hash.to_string().as_str());
            record.push_field(g.files.len().to_string().as_str());
            for f in g.files.iter() {
                record.push_field(format!("{}", f).as_ref());
            }
            wtr.write_record(&record)?;
        }
        wtr.flush()
    }

    /// Writes results as JSON.
    /// # Example output
    /// ```json
    /// {
    ///   "header": {
    ///     "version": "0.12.0",
    ///     "timestamp": "2021-05-03T13:20:59.285409824+00:00",
    ///     "command": [
    ///       "target/debug/fclones",
    ///       "find",
    ///       ".",
    ///       "-f",
    ///       "JSON",
    ///       "-o",
    ///       "report.json"
    ///     ],
    ///     "stats": {
    ///       "group_count": 553,
    ///       "redundant_file_count": 4266,
    ///       "redundant_file_size": 271838709
    ///     }
    ///   },
    ///   "groups": [
    ///     {
    ///       "file_len": 6626689,
    ///       "file_hash": "5649a555c131508c4a757d9e14c4aea6",
    ///       "files": [
    ///         "/home/pkolaczk/Projekty/fclones/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.0/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.1/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.2/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.11.0/target/debug/deps/libregex_syntax-94c84f5600b85f6e.rmeta"
    ///       ]
    ///     },
    ///     {
    ///       "file_len": 5815999,
    ///       "file_hash": "f79ce189d76620fd921986943087dc3a",
    ///       "files": [
    ///         "/home/pkolaczk/Projekty/fclones/target/debug/deps/libserde-af05e0212e5def7d.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.0/target/debug/deps/libserde-af05e0212e5def7d.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.1/target/debug/deps/libserde-af05e0212e5def7d.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.10.2/target/debug/deps/libserde-af05e0212e5def7d.rmeta",
    ///         "/home/pkolaczk/Projekty/fclones/target/package/fclones-0.11.0/target/debug/deps/libserde-af05e0212e5def7d.rmeta"
    ///       ]
    ///     },
    ///      ...
    ///   ]
    /// }
    /// ```
    pub fn write_as_json<I, P>(&mut self, header: &ReportHeader, groups: I) -> io::Result<()>
    where
        I: IntoIterator<Item = P>,
        P: Serialize,
    {
        let report = SerializableReport {
            header,
            groups: IteratorWrapper(Cell::new(Some(groups))),
        };

        serde_json::to_writer_pretty(&mut self.out, &report)?;
        Ok(())
    }

    /// Writes the report in the format given by `format` parameter.
    pub fn write<I, G, P>(
        &mut self,
        format: OutputFormat,
        header: &ReportHeader,
        groups: I,
    ) -> io::Result<()>
    where
        I: IntoIterator<Item = G>,
        G: Borrow<FileGroup<P>> + Serialize,
        P: Display + Serialize,
    {
        match format {
            OutputFormat::Default => self.write_as_text(header, groups),
            OutputFormat::Fdupes => self.write_as_fdupes(header, groups),
            OutputFormat::Csv => self.write_as_csv(header, groups),
            OutputFormat::Json => self.write_as_json(header, groups),
        }
    }
}

/// Iterator over groups of files, read form the report
pub type GroupIterator = dyn FallibleIterator<Item = FileGroup<Path>, Error = io::Error> + Send;

/// Reads a report from a stream.
pub trait ReportReader {
    /// Reads the header. Must be called exactly once before reading the groups.
    /// Reports an io::Error with ErrorKind::InvalidData
    /// if the report header is malformed.
    fn read_header(&mut self) -> io::Result<ReportHeader>;

    /// Opens an iterator over groups.
    fn read_groups(self: Box<Self>) -> io::Result<Box<GroupIterator>>;
}

/// Iterates the contents of the report.
/// Each emitted item is a group of duplicate files.
pub struct TextReportIterator<R: BufRead> {
    stream: R,
    line_buf: String,
    stopped_on_error: bool,
}

/// Helper struct to encapsulate the data in the header before each group of identical files
#[derive(Debug, Eq, PartialEq, Serialize)]
struct GroupHeader {
    count: usize,
    file_len: FileLen,
    file_hash: FileHash,
}

impl<R> TextReportIterator<R>
where
    R: BufRead,
{
    fn new(input: R) -> TextReportIterator<R> {
        TextReportIterator {
            stream: input,
            line_buf: String::new(),
            stopped_on_error: false,
        }
    }

    fn read_first_non_comment_line(&mut self) -> io::Result<Option<&str>> {
        loop {
            self.line_buf.clear();
            self.stream.read_line(&mut self.line_buf)?;
            let line = self.line_buf.trim();
            if line.is_empty() {
                return Ok(None);
            }
            if !line.starts_with('#') {
                break;
            }
        }
        Ok(Some(self.line_buf.trim()))
    }

    fn read_group_header(&mut self) -> io::Result<Option<GroupHeader>> {
        let header_str = match self.read_first_non_comment_line()? {
            None => return Ok(None),
            Some(s) => s,
        };

        lazy_static! {
            static ref GROUP_HEADER_RE: Regex =
                Regex::new(r"^([a-f0-9]{32}), ([0-9]+) B [^*]* \* ([0-9]+):").unwrap();
        }

        let captures = GROUP_HEADER_RE.captures(header_str).ok_or_else(|| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Malformed group header: {}", header_str),
            )
        })?;

        Ok(Some(GroupHeader {
            file_hash: FileHash(
                u128::from_str_radix(captures.get(1).unwrap().as_str(), 16).unwrap(),
            ),
            file_len: FileLen(captures.get(2).unwrap().as_str().parse::<u64>().unwrap()),
            count: captures.get(3).unwrap().as_str().parse::<usize>().unwrap(),
        }))
    }

    fn read_paths(&mut self, count: usize) -> io::Result<Vec<Path>> {
        let mut paths = Vec::with_capacity(min(count, 1024));
        for _ in 0..count {
            self.line_buf.clear();
            let n = self.stream.read_line(&mut self.line_buf)?;
            let path_str = &self.line_buf;
            if n == 0 {
                return Err(Error::new(
                    ErrorKind::UnexpectedEof,
                    "Unexpected end of file.",
                ));
            }
            if !path_str.starts_with("    ") || path_str.trim().is_empty() {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    format!("Path expected: {}", path_str),
                ));
            }
            paths.push(Path::from(path_str.trim()));
        }
        Ok(paths)
    }
}

impl<R: BufRead + 'static> FallibleIterator for TextReportIterator<R> {
    type Item = FileGroup<Path>;
    type Error = std::io::Error;

    fn next(&mut self) -> Result<Option<Self::Item>, Self::Error> {
        if self.stopped_on_error {
            return Ok(None);
        }
        match self.read_group_header() {
            Ok(Some(header)) => {
                let paths = self.read_paths(header.count)?;
                Ok(Some(FileGroup {
                    file_len: header.file_len,
                    file_hash: header.file_hash,
                    files: paths,
                }))
            }
            Ok(None) => Ok(None),
            Err(e) => {
                self.stopped_on_error = true;
                Err(e)
            }
        }
    }
}

/// Reads a text report from a stream.
///
/// Currently supports only the default text report format.
/// Does not load the whole report into memory.
/// Allows iterating over groups of files.
pub struct TextReportReader<R: BufRead> {
    pub stream: R,
}

impl<R: BufRead> TextReportReader<R> {
    /// Creates a new reader for reading from the given stream
    pub fn new(stream: R) -> TextReportReader<R> {
        TextReportReader { stream }
    }

    fn read_line(&mut self) -> io::Result<String> {
        let mut line_buf = String::new();
        self.stream.read_line(&mut line_buf)?;
        Ok(line_buf)
    }

    fn read_extract(&mut self, regex: &Regex, msg: &str) -> io::Result<Vec<String>> {
        let line = self.read_line()?;
        Ok(regex
            .captures(line.trim())
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, msg.to_owned()))?
            .iter()
            .skip(1)
            .map(|c| c.unwrap().as_str().to_owned())
            .collect())
    }
}

impl<R: BufRead + Send + 'static> ReportReader for TextReportReader<R> {
    fn read_header(&mut self) -> io::Result<ReportHeader> {
        lazy_static! {
            static ref VERSION_RE: Regex =
                Regex::new(r"^# Report by fclones ([0-9]+\.[0-9]+\.[0-9]+)").unwrap();
            static ref TIMESTAMP_RE: Regex = Regex::new(r"^# Timestamp: (.*)").unwrap();
            static ref COMMAND_RE: Regex = Regex::new(r"^# Command: (.*)").unwrap();
            static ref GROUP_COUNT_RE: Regex =
                Regex::new(r"^# Found ([0-9]+) file groups").unwrap();
            static ref STATS_RE: Regex =
                Regex::new(r"^# ([0-9]+) B \([^)]+\) in ([0-9]+) redundant files can be removed")
                    .unwrap();
        }

        let version = self
            .read_extract(&VERSION_RE, "Malformed header: Missing fclones version")?
            .swap_remove(0);
        let timestamp = self
            .read_extract(&TIMESTAMP_RE, "Malformed header: Missing timestamp")?
            .swap_remove(0);
        let timestamp = DateTime::parse_from_str(&timestamp, TIMESTAMP_FMT).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Malformed header: Failed to parse timestamp: {}", e),
            )
        })?;
        let command = self
            .read_extract(&COMMAND_RE, "Malformed header: Missing command")?
            .swap_remove(0);
        let command = shell_words::split(&command).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Malformed header: Failed to parse command arguments: {}", e),
            )
        })?;
        let group_count = self
            .read_extract(&GROUP_COUNT_RE, "Malformed header: Missing group count")?
            .swap_remove(0);
        let group_count: usize = group_count.parse().map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Malformed header: Failed to parse group count: {}", e),
            )
        })?;
        let stats_line =
            self.read_extract(&STATS_RE, "Malformed header: Missing file statistics line")?;
        let redundant_file_size = FileLen(stats_line[0].parse().map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Malformed header: Failed to parse file size {}: {}",
                    stats_line[0], e
                ),
            )
        })?);
        let redundant_file_count: usize = stats_line[1].parse().map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!(
                    "Malformed header: Failed to parse file count {}: {}",
                    stats_line[1], e
                ),
            )
        })?;

        Ok(ReportHeader {
            version,
            timestamp,
            command,
            stats: Some(FileStats {
                group_count,
                redundant_file_count,
                redundant_file_size,
            }),
        })
    }

    fn read_groups(
        self: Box<Self>,
    ) -> io::Result<Box<dyn FallibleIterator<Item = FileGroup<Path>, Error = Error> + Send>> {
        Ok(Box::new(TextReportIterator::new(self.stream)))
    }
}

/// Reads a report from a JSON file.
/// Currently it is not very memory efficient, because limited to reading the whole file and
/// deserializing all data into memory.
pub struct JsonReportReader {
    report: DeserializedReport,
}

impl JsonReportReader {
    pub fn new<R: Read>(stream: R) -> io::Result<JsonReportReader> {
        let report: DeserializedReport = serde_json::from_reader(stream).map_err(|e| {
            Error::new(
                ErrorKind::InvalidData,
                format!("Failed to deserialize JSON report: {}", e),
            )
        })?;
        Ok(JsonReportReader { report })
    }
}

impl ReportReader for JsonReportReader {
    fn read_header(&mut self) -> io::Result<ReportHeader> {
        Ok(self.report.header.clone())
    }

    fn read_groups(self: Box<Self>) -> io::Result<Box<GroupIterator>> {
        let iter = self.report.groups.into_iter().map(|g| {
            Ok(FileGroup {
                file_len: g.file_len,
                file_hash: g.file_hash,
                files: g.files.iter().map(|s| Path::from(s.as_str())).collect_vec(),
            })
        });
        let iter = fallible_iterator::convert(iter);
        Ok(Box::new(iter))
    }
}

/// Returns a `ReportReader` that can read and decode the report from the given stream.
/// Automatically detects the type of the report.
pub fn open_report(r: impl Read + Send + 'static) -> io::Result<Box<dyn ReportReader>> {
    let mut buf_reader = BufReader::with_capacity(16 * 1024, r);
    let preview = buf_reader.fill_buf()?;
    let preview = String::from_utf8_lossy(preview);
    if preview.starts_with('{') {
        Ok(Box::new(JsonReportReader::new(buf_reader)?))
    } else if preview.starts_with('#') {
        Ok(Box::new(TextReportReader::new(buf_reader)))
    } else {
        Err(io::Error::new(
            ErrorKind::InvalidData,
            format!(
                "Unknown report format. Supported formats are: {}, {}",
                OutputFormat::Default,
                OutputFormat::Json
            ),
        ))
    }
}

#[cfg(test)]
mod test {
    use tempfile::NamedTempFile;

    use crate::files::{FileHash, FileLen};
    use crate::path::Path;

    use super::*;

    fn dummy_report_header() -> ReportHeader {
        ReportHeader {
            command: vec!["fclones".to_owned(), "find".to_owned(), ".".to_owned()],
            version: env!("CARGO_PKG_VERSION").to_owned(),
            timestamp: DateTime::parse_from_str("2021-08-27 12:11:23.456 +0000", TIMESTAMP_FMT)
                .unwrap(),
            stats: Some(FileStats {
                group_count: 4,
                redundant_file_count: 234,
                redundant_file_size: FileLen(1000),
            }),
        }
    }

    #[test]
    fn test_text_report_reader_reads_header() {
        let header1 = dummy_report_header();
        let groups: Vec<FileGroup<Path>> = vec![];

        let output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();

        let mut writer = ReportWriter::new(output, false);
        writer.write_as_text(&header1, groups.into_iter()).unwrap();

        let mut reader = TextReportReader::new(BufReader::new(input));
        let header2 = reader.read_header().unwrap();
        assert_eq!(header2.version, header1.version);
        assert_eq!(header2.command, header1.command);
        assert_eq!(header2.timestamp.timestamp(), header1.timestamp.timestamp());
        assert_eq!(header2.stats, header1.stats);
    }

    #[test]
    fn test_text_report_reader_reads_files() {
        let header = dummy_report_header();
        let groups = vec![
            FileGroup {
                file_len: FileLen(100),
                file_hash: FileHash(0x00112233445566778899aabbccddeeff),
                files: vec![Path::from("a"), Path::from("b")],
            },
            FileGroup {
                file_len: FileLen(40),
                file_hash: FileHash(0x0000000000000555555555ffffffffff),
                files: vec![Path::from("c"), Path::from("d")],
            },
        ];

        let output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();

        let mut writer = ReportWriter::new(output, false);
        writer.write_as_text(&header, groups.iter()).unwrap();
        let mut reader = Box::new(TextReportReader::new(BufReader::new(input)));
        reader.read_header().unwrap();

        let groups2: Vec<_> = reader.read_groups().unwrap().collect().unwrap();
        assert_eq!(groups, groups2);
    }

    #[test]
    fn test_text_report_iterator_stops_on_error() {
        let mut output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();
        writeln!(output, "7d6ebf613bf94dfd976d169ff6ae02c3, 4 B (4 B) * 2:").unwrap();
        writeln!(output, "    /file1").unwrap();
        writeln!(output, "    /file2").unwrap();
        writeln!(output, "malformed group header:").unwrap();
        writeln!(output, "    /file1").unwrap();
        writeln!(output, "    /file2").unwrap();
        drop(output);

        let mut group_iterator = TextReportIterator::new(BufReader::new(input));
        assert!(group_iterator.next().is_ok());
        assert!(group_iterator.next().is_err());
        assert!(group_iterator.next().unwrap().is_none());
    }

    #[test]
    fn test_text_report_iterator_handles_windows_endlines() {
        let mut output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();
        write!(
            output,
            "7d6ebf613bf94dfd976d169ff6ae02c3, 4 B (4 B) * 2:\r\n"
        )
        .unwrap();
        write!(output, "    /file1\r\n").unwrap();
        write!(output, "    /file2\r\n").unwrap();
        write!(
            output,
            "7d6edf123096e5f4b7fcd002351faccc, 4 B (4 B) * 2:\r\n"
        )
        .unwrap();
        write!(output, "    /file3\r\n").unwrap();
        write!(output, "    /file4\r\n").unwrap();
        drop(output);

        let mut group_iterator = TextReportIterator::new(BufReader::new(input));
        let g = group_iterator.next().unwrap().unwrap();
        assert!(g.files.contains(&Path::from("/file1")));
        assert!(g.files.contains(&Path::from("/file2")));
        let g = group_iterator.next().unwrap().unwrap();
        assert!(g.files.contains(&Path::from("/file3")));
        assert!(g.files.contains(&Path::from("/file4")));
    }

    #[test]
    fn test_json_report_header() {
        let header1 = dummy_report_header();
        let groups: Vec<FileGroup<Path>> = vec![];

        let output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();

        let mut writer = ReportWriter::new(output, false);
        writer.write_as_json(&header1, groups.into_iter()).unwrap();

        let mut reader = JsonReportReader::new(input).unwrap();
        let header2 = reader.read_header().unwrap();
        assert_eq!(header2.version, header1.version);
        assert_eq!(header2.command, header1.command);
        assert_eq!(header2.timestamp.timestamp(), header1.timestamp.timestamp());
        assert_eq!(header2.stats, header1.stats);
    }

    #[test]
    fn test_json_report_reader_reads_files() {
        let header = dummy_report_header();
        let groups = vec![
            FileGroup {
                file_len: FileLen(100),
                file_hash: FileHash(0x00112233445566778899aabbccddeeff),
                files: vec![Path::from("a"), Path::from("b")],
            },
            FileGroup {
                file_len: FileLen(40),
                file_hash: FileHash(0x0000000000000555555555ffffffffff),
                files: vec![Path::from("c"), Path::from("d")],
            },
        ];

        let output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();

        let mut writer = ReportWriter::new(output, false);
        writer.write_as_json(&header, groups.iter()).unwrap();
        let mut reader = Box::new(JsonReportReader::new(input).unwrap());
        reader.read_header().unwrap();

        let groups2: Vec<_> = reader.read_groups().unwrap().collect().unwrap();
        assert_eq!(groups, groups2);
    }

    fn write_read_header(header: &ReportHeader, format: OutputFormat) -> ReportHeader {
        let groups: Vec<FileGroup<Path>> = vec![];
        let output = NamedTempFile::new().unwrap();
        let input = output.reopen().unwrap();
        let mut writer = ReportWriter::new(output, false);
        writer.write(format, header, groups.iter()).unwrap();
        let mut reader = open_report(input).unwrap();
        reader.read_header().unwrap()
    }

    #[test]
    fn test_format_autodetection() {
        let header = dummy_report_header();
        let reread_header_1 = write_read_header(&header, OutputFormat::Default);
        let reread_header_2 = write_read_header(&header, OutputFormat::Json);
        assert_eq!(header, reread_header_1);
        assert_eq!(header, reread_header_2);
    }
}
