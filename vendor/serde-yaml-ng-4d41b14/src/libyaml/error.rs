use libyaml_safer::{Error as SaferError, ErrorKind, Mark as SaferMark};
use std::fmt::{self, Debug, Display};

pub(crate) type Result<T> = std::result::Result<T, Error>;

pub(crate) struct Error {
    kind: ErrorKind,
    problem: String,
    problem_offset: u64,
    problem_mark: Mark,
    context: Option<String>,
    context_mark: Mark,
}

impl Error {
    pub fn from_safer_error(err: SaferError) -> Self {
        let problem = err.problem().to_string();
        let context = err.context().map(|s| s.to_string());
        let problem_mark = Mark::from_safer_mark(err.problem_mark().unwrap_or_default());
        let context_mark = Mark::from_safer_mark(err.context_mark().unwrap_or_default());

        Error {
            kind: err.kind(),
            problem,
            problem_offset: problem_mark.index(),
            problem_mark,
            context,
            context_mark,
        }
    }

    pub fn mark(&self) -> Mark {
        self.problem_mark
    }
}

impl Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        write!(formatter, "{}", self.problem)?;
        if self.problem_mark.line() != 0 || self.problem_mark.column() != 0 {
            write!(formatter, " at {}", self.problem_mark)?;
        } else if self.problem_offset != 0 {
            write!(formatter, " at position {}", self.problem_offset)?;
        }
        if let Some(context) = &self.context {
            write!(formatter, ", {}", context)?;
            if (self.context_mark.line() != 0 || self.context_mark.column() != 0)
                && (self.context_mark.line() != self.problem_mark.line()
                    || self.context_mark.column() != self.problem_mark.column())
            {
                write!(formatter, " at {}", self.context_mark)?;
            }
        }
        Ok(())
    }
}

impl Debug for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let mut formatter = formatter.debug_struct("Error");
        let kind_str = match self.kind {
            ErrorKind::Reader => "READER",
            ErrorKind::Scanner => "SCANNER",
            ErrorKind::Parser => "PARSER",
            ErrorKind::Composer => "COMPOSER",
            ErrorKind::Emitter => "EMITTER",
            ErrorKind::Io => "IO",
        };
        formatter.field("kind", &format_args!("{}", kind_str));
        formatter.field("problem", &self.problem);
        if self.problem_mark.line() != 0 || self.problem_mark.column() != 0 {
            formatter.field("problem_mark", &self.problem_mark);
        } else if self.problem_offset != 0 {
            formatter.field("problem_offset", &self.problem_offset);
        }
        if let Some(context) = &self.context {
            formatter.field("context", context);
            if self.context_mark.line() != 0 || self.context_mark.column() != 0 {
                formatter.field("context_mark", &self.context_mark);
            }
        }
        formatter.finish()
    }
}

#[derive(Copy, Clone)]
pub(crate) struct Mark {
    index: u64,
    line: u64,
    column: u64,
}

impl Mark {
    pub fn from_safer_mark(mark: SaferMark) -> Self {
        Mark {
            index: mark.index,
            line: mark.line,
            column: mark.column,
        }
    }

    pub fn index(&self) -> u64 {
        self.index
    }

    pub fn line(&self) -> u64 {
        self.line
    }

    pub fn column(&self) -> u64 {
        self.column
    }
}

impl Display for Mark {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        if self.line != 0 || self.column != 0 {
            write!(
                formatter,
                "line {} column {}",
                self.line + 1,
                self.column + 1,
            )
        } else {
            write!(formatter, "position {}", self.index)
        }
    }
}

impl Debug for Mark {
    fn fmt(&self, formatter: &mut fmt::Formatter) -> fmt::Result {
        let mut formatter = formatter.debug_struct("Mark");
        if self.line != 0 || self.column != 0 {
            formatter.field("line", &(self.line + 1));
            formatter.field("column", &(self.column + 1));
        } else {
            formatter.field("index", &self.index);
        }
        formatter.finish()
    }
}
