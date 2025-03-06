use std::collections::HashMap;
use std::io::{self, Error, Write};
use std::sync::Arc;

use jj_cli::formatter::{Formatter, Style};
use jj_lib::config::{ConfigGetError, StackedConfig};

type Rules = Vec<(Vec<String>, Style)>;

#[derive(Clone, Debug)]
pub struct ColorFormatter<W: Write> {
    output: W,
    rules: Arc<Rules>,
    /// The stack of currently applied labels. These determine the desired
    /// style.
    labels: Vec<String>,
    cached_styles: HashMap<Vec<String>, Style>,
    /// The style we last wrote to the output.
    current_style: Style,
    /// The debug string (space-separated labels) we last wrote to the output.
    /// Initialize to None to turn debug strings off.
    current_debug: Option<String>,
}

impl<W: Write> ColorFormatter<W> {
    pub fn new(output: W, rules: Arc<Rules>, debug: bool) -> ColorFormatter<W> {
        ColorFormatter {
            output,
            rules,
            labels: vec![],
            cached_styles: HashMap::new(),
            current_style: Style::default(),
            current_debug: debug.then(String::new),
        }
    }

    pub fn for_config(output: W, config: &StackedConfig, debug: bool) -> Result<Self, ConfigGetError> {
        let rules = jj_cli::formatter::rules_from_config(config)?;
        Ok(Self::new(output, Arc::new(rules), debug))
    }

    fn requested_style(&mut self) -> Style {
        if let Some(cached) = self.cached_styles.get(&self.labels) {
            cached.clone()
        } else {
            // We use the reverse list of matched indices as a measure of how well the rule
            // matches the actual labels. For example, for rule "a d" and the actual labels
            // "a b c d", we'll get [3,0]. We compare them by Rust's default Vec comparison.
            // That means "a d" will trump both rule "d" (priority [3]) and rule
            // "a b c" (priority [2,1,0]).
            let mut matched_styles = vec![];
            for (labels, style) in self.rules.as_ref() {
                let mut labels_iter = self.labels.iter().enumerate();
                // The indexes in the current label stack that match the required label.
                let mut matched_indices = vec![];
                for required_label in labels {
                    for (label_index, label) in &mut labels_iter {
                        if label == required_label {
                            matched_indices.push(label_index);
                            break;
                        }
                    }
                }
                if matched_indices.len() == labels.len() {
                    matched_indices.reverse();
                    matched_styles.push((style, matched_indices));
                }
            }
            matched_styles.sort_by_key(|(_, indices)| indices.clone());

            let mut style = Style::default();
            for (matched_style, _) in matched_styles {
                style.merge(matched_style);
            }
            self.cached_styles.insert(self.labels.clone(), style.clone());
            style
        }
    }

    fn write_new_style(&mut self) -> io::Result<()> {
        let new_debug = match &self.current_debug {
            Some(current) => {
                let joined = self.labels.join(" ");
                if joined == *current {
                    None
                } else {
                    if !current.is_empty() {
                        write!(self.output, ">>")?;
                    }
                    Some(joined)
                }
            }
            None => None,
        };
        let new_style = self.requested_style();
        if new_style != self.current_style {
            if new_style.bold != self.current_style.bold {
                if new_style.bold.unwrap_or_default() {
                    queue!(self.output, SetAttribute(Attribute::Bold))?;
                } else {
                    // NoBold results in double underlining on some terminals, so we use reset
                    // instead. However, that resets other attributes as well, so we reset
                    // our record of the current style so we re-apply the other attributes
                    // below.
                    queue!(self.output, SetAttribute(Attribute::Reset))?;
                    self.current_style = Style::default();
                }
            }
            if new_style.italic != self.current_style.italic {
                if new_style.italic.unwrap_or_default() {
                    queue!(self.output, SetAttribute(Attribute::Italic))?;
                } else {
                    queue!(self.output, SetAttribute(Attribute::NoItalic))?;
                }
            }
            if new_style.underline != self.current_style.underline {
                if new_style.underline.unwrap_or_default() {
                    queue!(self.output, SetAttribute(Attribute::Underlined))?;
                } else {
                    queue!(self.output, SetAttribute(Attribute::NoUnderline))?;
                }
            }
            if new_style.fg != self.current_style.fg {
                queue!(self.output, SetForegroundColor(new_style.fg.unwrap_or(Color::Reset)))?;
            }
            if new_style.bg != self.current_style.bg {
                queue!(self.output, SetBackgroundColor(new_style.bg.unwrap_or(Color::Reset)))?;
            }
            self.current_style = new_style;
        }
        if let Some(d) = new_debug {
            if !d.is_empty() {
                write!(self.output, "<<{d}::")?;
            }
            self.current_debug = Some(d);
        }
        Ok(())
    }
}

impl<W: Write> Write for ColorFormatter<W> {
    fn write(&mut self, data: &[u8]) -> Result<usize, Error> {
        /*
        We clear the current style at the end of each line, and then we re-apply the style
        after the newline. There are several reasons for this:

         * We can more easily skip styling a trailing blank line, which other
           internal code then can correctly detect as having a trailing
           newline.

         * Some tools (like `less -R`) add an extra newline if the final
           character is not a newline (e.g. if there's a color reset after
           it), which led to an annoying blank line after the diff summary in
           e.g. `jj status`.

         * Since each line is styled independently, you get all the necessary
           escapes even when grepping through the output.

         * Some terminals extend background color to the end of the terminal
           (i.e. past the newline character), which is probably not what the
           user wanted.

         * Some tools (like `less -R`) get confused and lose coloring of lines
           after a newline.
         */

        for line in data.split_inclusive(|b| *b == b'\n') {
            if line.ends_with(b"\n") {
                self.write_new_style()?;
                write_sanitized(&mut self.output, &line[..line.len() - 1])?;
                let labels = mem::take(&mut self.labels);
                self.write_new_style()?;
                self.output.write_all(b"\n")?;
                self.labels = labels;
            } else {
                self.write_new_style()?;
                write_sanitized(&mut self.output, line)?;
            }
        }

        Ok(data.len())
    }

    fn flush(&mut self) -> Result<(), Error> {
        self.output.flush()
    }
}

impl<W: Write> Formatter for ColorFormatter<W> {
    fn raw(&mut self) -> io::Result<Box<dyn Write + '_>> {
        self.write_new_style()?;
        Ok(Box::new(self.output.by_ref()))
    }

    fn push_label(&mut self, label: &str) -> io::Result<()> {
        self.labels.push(label.to_owned());
        Ok(())
    }

    fn pop_label(&mut self) -> io::Result<()> {
        self.labels.pop();
        if self.labels.is_empty() {
            self.write_new_style()?;
        }
        Ok(())
    }
}

impl<W: Write> Drop for ColorFormatter<W> {
    fn drop(&mut self) {
        // If a `ColorFormatter` was dropped without popping all labels first (perhaps
        // because of an error), let's still try to reset any currently active style.
        self.labels.clear();
        self.write_new_style().ok();
    }
}
