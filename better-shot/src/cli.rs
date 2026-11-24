//! Command-line parsing, kept in the library so it can be tested without a
//! display or a running niri.

use std::path::PathBuf;

use crate::annotate::Annotation;
use crate::hdr::HdrRequest;
use crate::output::OutputFormat;
use crate::record::RecordOptions;
use crate::{Mode, Options};

pub const USAGE: &str = r#"better-shot [--socket PATH] <ui|screen|window> [options]
better-shot record [options]        (see `better-shot record --help`)

  Options:
    --path PATH         Save here (default: Screenshots/better-shot-<epoch>.<ext>)
    --id ID             Window id for `window` (default: focused)
    --no-pointer        Hide the pointer
    --pointer           Show the pointer (default)
    --copy              Pipe the saved image to wl-copy (always as PNG)
    --annotate          Open the saved image in satty (interactive)
    --annotate=SPEC     Draw an annotation without a GUI (repeatable):
                          rect:x,y,w,h
                          text:x,y,STRING
                          arrow:x1,y1,x2,y2
    --hdr=MODE          keep (default) | sdr | tonemap
    --format=FORMAT     png | jpeg | webp (default: from --path, else png)
    --quality N         JPEG quality 1-100 (default: 90)
    --metadata          Write a JSON sidecar next to the image
    --dry-run           Print the planned action without capturing

  Socket defaults to $NIRI_SOCKET.
"#;

#[derive(Debug, Clone, PartialEq)]
pub struct ParsedCli {
    pub socket: Option<PathBuf>,
    pub options: Options,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CliAction {
    Run(ParsedCli),
    Record(RecordOptions),
    Help,
}

/// Parses arguments *without* `argv[0]`.
pub fn parse<I, S>(args: I) -> Result<CliAction, String>
where
    I: IntoIterator<Item = S>,
    S: Into<String>,
{
    let args: Vec<String> = args.into_iter().map(Into::into).collect();
    if let Some(index) = record_mode_index(&args) {
        let remaining = args
            .iter()
            .enumerate()
            .filter(|(candidate, _)| *candidate != index)
            .map(|(_, arg)| arg.clone())
            .collect::<Vec<_>>();
        return Ok(match crate::record::parse_cli(remaining)? {
            crate::record::RecordCli::Run(options) => CliAction::Record(options),
            crate::record::RecordCli::Help => CliAction::Help,
        });
    }
    let mut iter = args.into_iter().peekable();
    let mut socket = None;
    let mut options = Options::default();
    let mut mode = None;

    while let Some(raw) = iter.next() {
        let (flag, inline) = match raw.split_once('=') {
            Some((flag, value)) => (flag.to_owned(), Some(value.to_owned())),
            None => (raw, None),
        };
        match flag.as_str() {
            "--socket" => {
                socket = Some(PathBuf::from(value(flag.as_str(), inline, &mut iter)?));
            }
            "--path" => {
                options.path = Some(PathBuf::from(value(flag.as_str(), inline, &mut iter)?));
            }
            "--id" => {
                let value = value(flag.as_str(), inline, &mut iter)?;
                options.window_id = Some(
                    value
                        .parse()
                        .map_err(|_| format!("--id needs a number, got `{value}`"))?,
                );
            }
            "--no-pointer" => {
                no_value(flag.as_str(), inline)?;
                options.show_pointer = false;
            }
            "--pointer" => {
                no_value(flag.as_str(), inline)?;
                options.show_pointer = true;
            }
            "--copy" => {
                no_value(flag.as_str(), inline)?;
                options.copy = true;
            }
            "--annotate" => match inline {
                Some(spec) => options.annotations.push(Annotation::parse(&spec)?),
                None => options.annotate = true,
            },
            "--hdr" => {
                let value = value(flag.as_str(), inline, &mut iter)?;
                options.hdr = HdrRequest::parse(&value)
                    .ok_or_else(|| format!("--hdr needs keep, sdr or tonemap, got `{value}`"))?;
            }
            "--format" => {
                let value = value(flag.as_str(), inline, &mut iter)?;
                options.format =
                    Some(OutputFormat::parse(&value).ok_or_else(|| {
                        format!("--format needs png, jpeg or webp, got `{value}`")
                    })?);
            }
            "--quality" => {
                let value = value(flag.as_str(), inline, &mut iter)?;
                let quality: u8 = value
                    .parse()
                    .map_err(|_| format!("--quality needs a number 1-100, got `{value}`"))?;
                if !(1..=100).contains(&quality) {
                    return Err(format!("--quality needs a number 1-100, got `{value}`"));
                }
                options.quality = quality;
            }
            "--metadata" => {
                no_value(flag.as_str(), inline)?;
                options.metadata = true;
            }
            "--dry-run" => {
                no_value(flag.as_str(), inline)?;
                options.dry_run = true;
            }
            "--help" | "-h" => return Ok(CliAction::Help),
            "ui" | "screen" | "window" => {
                if inline.is_some() {
                    return Err(format!("`{flag}` is a mode, not an option"));
                }
                mode = Some(flag);
            }
            other if other.starts_with('-') => {
                return Err(format!("unknown option `{other}`\n\n{USAGE}"));
            }
            other => {
                return Err(format!("unexpected argument `{other}`\n\n{USAGE}"));
            }
        }
    }

    options.mode = match mode.as_deref() {
        Some("screen") => Mode::Screen,
        Some("window") => Mode::Window,
        Some("ui") | None => Mode::Ui,
        _ => unreachable!(),
    };
    if options.mode == Mode::Ui
        && (options.copy
            || options.annotate
            || !options.annotations.is_empty()
            || options.metadata
            || options.format.is_some()
            || options.hdr != HdrRequest::Keep)
    {
        return Err(
            "--copy/--annotate/--metadata/--format/--hdr are not supported with the \
             interactive UI mode (it saves asynchronously)"
                .to_owned(),
        );
    }

    Ok(CliAction::Run(ParsedCli { socket, options }))
}

fn record_mode_index(args: &[String]) -> Option<usize> {
    let takes_value = |flag: &str| {
        matches!(
            flag,
            "--socket"
                | "--path"
                | "--id"
                | "--hdr"
                | "--format"
                | "--quality"
                | "--output"
                | "--region"
                | "--window"
                | "--fps"
                | "--codec"
                | "--audio"
                | "--duration"
                | "--backend"
                | "--command"
        )
    };
    let mut index = 0;
    while index < args.len() {
        let arg = &args[index];
        if arg.starts_with('-') {
            index += 1;
            if !arg.contains('=') && takes_value(arg) {
                index += 1;
            }
            continue;
        }
        return (arg == "record").then_some(index);
    }
    None
}

fn value<I>(
    flag: &str,
    inline: Option<String>,
    iter: &mut std::iter::Peekable<I>,
) -> Result<String, String>
where
    I: Iterator<Item = String>,
{
    if let Some(value) = inline {
        if value.is_empty() {
            return Err(format!("{flag} needs a value"));
        }
        return Ok(value);
    }
    match iter.next() {
        Some(value) if !value.starts_with("--") => Ok(value),
        _ => Err(format!("{flag} needs a value")),
    }
}

fn no_value(flag: &str, inline: Option<String>) -> Result<(), String> {
    if inline.is_some() {
        Err(format!("{flag} does not take a value"))
    } else {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn run(args: &[&str]) -> ParsedCli {
        match parse(args.iter().copied()).unwrap() {
            CliAction::Run(cli) => cli,
            CliAction::Record(_) => panic!("expected a screenshot run, got a recording"),
            CliAction::Help => panic!("expected a run, got help"),
        }
    }

    #[test]
    fn defaults_to_ui_and_keep() {
        let cli = run(&[]);
        assert_eq!(cli.options.mode, Mode::Ui);
        assert_eq!(cli.options.hdr, HdrRequest::Keep);
        assert_eq!(cli.options.quality, 90);
        assert!(cli.options.show_pointer);
        assert!(!cli.options.copy);
        assert!(cli.options.annotations.is_empty());
        assert!(cli.socket.is_none());
    }

    #[test]
    fn parses_full_screen_invocation() {
        let cli = run(&[
            "screen",
            "--socket",
            "/run/niri.sock",
            "--path=/tmp/a.png",
            "--no-pointer",
            "--copy",
            "--hdr",
            "sdr",
            "--format",
            "webp",
            "--quality=80",
            "--metadata",
            "--annotate=rect:1,2,3,4",
            "--annotate=text:5,6,hello, world",
            "--annotate=arrow:7,8,9,10",
            "--dry-run",
        ]);
        assert_eq!(
            cli.socket.as_deref(),
            Some(std::path::Path::new("/run/niri.sock"))
        );
        assert_eq!(cli.options.mode, Mode::Screen);
        assert_eq!(
            cli.options.path.as_deref(),
            Some(std::path::Path::new("/tmp/a.png"))
        );
        assert!(!cli.options.show_pointer);
        assert!(cli.options.copy);
        assert!(cli.options.metadata);
        assert!(cli.options.dry_run);
        assert_eq!(cli.options.hdr, HdrRequest::Sdr);
        assert_eq!(cli.options.format, Some(OutputFormat::WebP));
        assert_eq!(cli.options.quality, 80);
        assert_eq!(
            cli.options.annotations,
            vec![
                Annotation::Rect {
                    x: 1,
                    y: 2,
                    w: 3,
                    h: 4
                },
                Annotation::Text {
                    x: 5,
                    y: 6,
                    text: "hello, world".to_owned()
                },
                Annotation::Arrow {
                    x1: 7,
                    y1: 8,
                    x2: 9,
                    y2: 10
                },
            ]
        );
    }

    #[test]
    fn separates_satty_from_builtin_annotations() {
        let cli = run(&["window", "--id", "42", "--annotate"]);
        assert!(cli.options.annotate);
        assert!(cli.options.annotations.is_empty());
        assert_eq!(cli.options.window_id, Some(42));
        assert_eq!(cli.options.mode, Mode::Window);
    }

    #[test]
    fn help_is_not_a_run() {
        assert_eq!(parse(["--help"]).unwrap(), CliAction::Help);
        assert_eq!(parse(["-h"]).unwrap(), CliAction::Help);
    }

    #[test]
    fn rejects_bad_input() {
        for args in [
            vec!["screen", "--hdr", "sometimes"],
            vec!["screen", "--format", "bmp"],
            vec!["screen", "--quality", "0"],
            vec!["screen", "--quality", "101"],
            vec!["screen", "--quality"],
            vec!["screen", "--id", "nope"],
            vec!["screen", "--annotate=rect:1,2,3"],
            vec!["screen", "--nope"],
            vec!["screen", "extra"],
            vec!["screen", "--copy=yes"],
            vec!["ui", "--copy"],
            vec!["ui", "--annotate"],
            vec!["ui", "--annotate=rect:1,2,3,4"],
            vec!["ui", "--metadata"],
            vec!["ui", "--format=webp"],
            vec!["ui", "--hdr=sdr"],
        ] {
            assert!(parse(args.clone()).is_err(), "accepted {args:?}");
        }
    }

    #[test]
    fn accepts_equals_and_space_forms() {
        let cli = run(&["screen", "--hdr=tonemap", "--format=jpeg", "--id=3"]);
        assert_eq!(cli.options.hdr, HdrRequest::Tonemap);
        assert_eq!(cli.options.format, Some(OutputFormat::Jpeg));
        assert_eq!(cli.options.window_id, Some(3));
    }

    #[test]
    fn delegates_record_mode_after_options() {
        let CliAction::Record(options) = parse([
            "--socket",
            "/run/niri.sock",
            "record",
            "--output",
            "DP-1",
            "--json",
        ])
        .unwrap() else {
            panic!("expected a record action");
        };
        assert_eq!(
            options.target,
            crate::record::Target::Output("DP-1".to_owned())
        );
        assert!(options.json);
        assert_eq!(
            options.socket.as_deref(),
            Some(std::path::Path::new("/run/niri.sock"))
        );

        let CliAction::Record(options) =
            parse(["record", "--region=0,0,64,64", "--codec=av1"]).unwrap()
        else {
            panic!("expected a record action");
        };
        assert_eq!(
            options.target,
            crate::record::Target::Region(crate::record::Region {
                x: 0,
                y: 0,
                w: 64,
                h: 64
            })
        );
        assert_eq!(options.codec, crate::record::Codec::Av1);

        assert_eq!(parse(["record", "--help"]).unwrap(), CliAction::Help);
    }

    #[test]
    fn record_word_as_an_option_value_is_not_a_mode() {
        // `--path record` must stay a screenshot invocation, not switch modes.
        let cli = run(&["--path", "record", "screen"]);
        assert_eq!(cli.options.mode, Mode::Screen);
        assert_eq!(
            cli.options.path.as_deref(),
            Some(std::path::Path::new("record"))
        );
    }
}
