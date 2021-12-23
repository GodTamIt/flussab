//! Parsing and writing of the GCNF file format for group oriented CNF formulas.
use std::io::{BufReader, Read, Write};

use flussab::{text::LineReader, write, DeferredReader, DeferredWriter};

use crate::{error::ParseError, token, Dimacs};

/// Header data of a GCNF file.
///
/// Contains the number of variables and clauses as well as the number of groups. This parser
/// considers a count of `0` as unspecified and will not check that count during parsing, even in
/// strict mode.
#[derive(Copy, Clone, Debug)]
pub struct Header {
    /// Upper bound on the number of variables present in the formula.
    ///
    /// Ignored during parsing when `0`.
    pub var_count: usize,
    /// Number of clauses present in the formula.
    ///
    /// Ignored during parsing when `0`.
    pub clause_count: usize,

    /// Number of clause groups.
    ///
    /// Note that this count does not include the don't-care group `0`.
    ///
    /// Ignored during parsing when `0`.
    pub group_count: usize,
}

/// Configuration for the GCNF parser.
#[derive(Clone, Default, Debug)]
#[non_exhaustive]
pub struct Config {
    /// When set, the variable, clause and group count of the GCNF header are ignored
    /// during parsing. (Default: `false`)
    pub ignore_header: bool,
}

impl Config {
    #[inline]
    /// Sets the [`ignore_header`][Self#structfield.ignore_header] field.
    pub fn ignore_header(mut self, value: bool) -> Self {
        self.ignore_header = value;
        self
    }
}

/// Parser for the GCNF file format.
pub struct Parser<'a, L> {
    reader: LineReader<'a>,
    clause_count: usize,
    clause_limit: usize,
    clause_limit_active: bool,
    lit_limit: isize,
    lit_limit_is_hard: bool,
    group_limit: usize,
    group_limit_is_hard: bool,
    lit_buf: Vec<L>,
    header: Option<Header>,
}

impl<'a, L> Parser<'a, L>
where
    L: Dimacs,
{
    /// Creates a parser reading from a [`BufReader`].
    pub fn from_buf_reader(
        buf_reader: BufReader<impl Read + 'a>,
        config: Config,
    ) -> Result<Self, ParseError> {
        Self::new(
            LineReader::new(DeferredReader::from_buf_reader(buf_reader)),
            config,
        )
    }

    /// Creates a parser reading from a [`Read`] instance.
    ///
    /// If the [`Read`] instance is a [`BufReader`], it is better to use
    /// [`from_buf_reader`][Self::from_buf_reader] to avoid unnecessary double buffering of the
    /// data.
    pub fn from_read(read: impl Read + 'a, config: Config) -> Result<Self, ParseError> {
        Self::new(LineReader::new(DeferredReader::from_read(read)), config)
    }

    /// Creates a parser reading from a boxed [`Read`] instance.
    ///
    /// If the [`Read`] instance is a [`BufReader`], it is better to use
    /// [`from_buf_reader`][Self::from_buf_reader] to avoid unnecessary double buffering of the
    /// data.
    #[inline(never)]
    pub fn from_boxed_dyn_read(
        read: Box<dyn Read + 'a>,
        config: Config,
    ) -> Result<Self, ParseError> {
        Self::new(
            LineReader::new(DeferredReader::from_boxed_dyn_read(read)),
            config,
        )
    }

    /// Creates a parser reading from a [`LineReader`].
    pub fn new(reader: LineReader<'a>, config: Config) -> Result<Self, ParseError> {
        let mut new = Self {
            reader,
            clause_count: 0,
            clause_limit: 0,
            clause_limit_active: false,
            lit_limit: L::MAX_DIMACS,
            lit_limit_is_hard: true,
            group_limit: usize::MAX,
            group_limit_is_hard: true,
            lit_buf: vec![],
            header: None,
        };

        if let Some(header) = new.parse_header()? {
            if !config.ignore_header {
                if header.var_count != 0 {
                    new.lit_limit = header.var_count as isize;
                    new.lit_limit_is_hard = false;
                }
                if header.clause_count != 0 {
                    new.clause_limit = header.clause_count;
                    new.clause_limit_active = true;
                }
                if header.group_count != 0 {
                    new.group_limit = header.group_count;
                    new.group_limit_is_hard = false;
                }
            }
            new.header = Some(header);
        }

        Ok(new)
    }

    fn parse_header(&mut self) -> Result<Option<Header>, ParseError> {
        let reader = &mut self.reader;

        token::skip_whitespace(reader);
        while token::comment(reader).matches()? || token::newline(reader).matches()? {}

        token::word(reader, b"p")
            .and_then(|_| {
                token::word(reader, b"gcnf")
                    .or_give_up(|| token::unexpected(reader, "\"gcnf\""))?;

                let var_count = token::var_count::<L>(reader)
                    .or_give_up(|| token::unexpected(reader, "variable count"))?;

                let clause_count = token::uint_count(reader, "clause count")
                    .or_give_up(|| token::unexpected(reader, "clause count"))?;

                let group_count = token::uint_count(reader, "group count")
                    .or_give_up(|| token::unexpected(reader, "group count"))?;

                token::interactive_end_of_line(reader)
                    .or_give_up(|| token::unexpected(reader, "end of line"))?;

                Ok(Header {
                    var_count,
                    clause_count,
                    group_count,
                })
            })
            .optional()
    }

    /// Returns the GCNF header if it was present.
    pub fn header(&self) -> Option<Header> {
        self.header
    }

    /// Parses and returns the next clause.
    ///
    /// The clause is returned as a pair of its group and its literals.
    ///
    /// Returns `Ok(None)` if the end of file was successfully reached.
    pub fn next_clause(&mut self) -> Result<Option<(usize, &[L])>, ParseError> {
        let mut input = &mut self.reader;
        self.lit_buf.clear();

        token::skip_whitespace(input);
        loop {
            if self.clause_count != self.clause_limit || !self.clause_limit_active {
                let group = token::clause_group(input, self.group_limit, self.group_limit_is_hard)
                    .and_also(|_| {
                        let input = &mut self.reader;
                        token::non_terminating_linebreaks(input)?;
                        token::clause_lits(
                            input,
                            &mut self.lit_buf,
                            self.lit_limit,
                            self.lit_limit_is_hard,
                        )
                        .or_give_up(|| token::unexpected(input, "clause literals"))?;
                        token::interactive_end_of_line(input)
                            .or_give_up(|| token::unexpected(input, "end of line"))
                    });

                if let Some(group) = group.optional()? {
                    self.clause_count += 1;
                    break Ok(Some((group, &self.lit_buf)));
                }
            }
            input = &mut self.reader;

            if token::comment(input).matches()? {
                continue;
            }

            if token::newline(input).matches()? {
                continue;
            }

            if (!self.clause_limit_active || self.clause_count >= self.clause_limit)
                && token::eof(input).matches()?
            {
                break Ok(None);
            }

            break Err(self.unexpected_statement());
        }
    }

    #[cold]
    #[inline(never)]
    fn unexpected_statement(&mut self) -> ParseError {
        let mut expected = vec![];

        if self.clause_count == 0 && self.header.is_none() {
            expected.push("header");
        }
        if !self.clause_limit_active || self.clause_count < self.clause_limit {
            expected.push("clause group");
        }
        expected.push("comment");

        if !self.clause_limit_active || self.clause_count >= self.clause_limit {
            expected.push("end of file");
        }

        let last = expected.pop().unwrap();
        let mut expected = expected.join(", ");
        expected.push_str(" or ");
        expected.push_str(last);

        token::unexpected(&mut self.reader, &expected)
    }
}

/// Writes a GCNF header.
pub fn write_header(writer: &mut DeferredWriter, header: Header) {
    let _ = writeln!(
        writer,
        "p gcnf {} {} {}",
        header.var_count, header.clause_count, header.group_count
    );
}

/// Writes a clause belonging to a group.
pub fn write_clause<L: Dimacs>(writer: &mut DeferredWriter, group: usize, clause_lits: &[L]) {
    writer.write_all_defer_err(b"{");
    write::text::ascii_digits(writer, group);
    writer.write_all_defer_err(b"} ");
    for lit in clause_lits {
        write::text::ascii_digits(writer, lit.dimacs());
        writer.write_all_defer_err(b" ");
    }
    writer.write_all_defer_err(b"0\n");
}

#[cfg(test)]
mod tests {
    use super::*;

    type Result<T> = std::result::Result<T, ParseError>;

    #[test]
    fn roundtrip() -> Result<()> {
        let input = &r"
p gcnf 5 12 3
{0} -1 -2 0
{0} -1 -3 0
{0} -1 -4 0
{0} -1 -5 0
{0} -2 -3 0
{0} -2 -4 0
{0} -2 -5 0
{1} -3 -4 0
{1} -3 -5 0
{2} -4 -5 0
{3} 2 5 3 4 1 0
{3} 4 2 3 1 5 0
"[1..];

        let mut output = vec![];
        let mut parser = Parser::<i32>::from_read(input.as_bytes(), Config::default())?;

        {
            let mut writer = DeferredWriter::from_write(&mut output);

            write_header(&mut writer, parser.header().unwrap());

            while let Some((group, clause)) = parser.next_clause()? {
                write_clause(&mut writer, group, clause);
            }

            writer.flush()?;
        }

        assert_eq!(input.as_bytes(), output);

        Ok(())
    }
}
