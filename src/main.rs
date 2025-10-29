use std::{
    io::{BufRead as _, BufReader},
    os::unix::process::CommandExt,
    path::{Path, PathBuf},
    process::{Command, Stdio, exit},
};

fn main() {
    if let Err(err) = try_main() {
        eprintln!("{err}");
        exit(1);
    }
}

fn try_main() -> Result<(), Box<dyn std::error::Error>> {
    let args: Args = argh::cargo_from_env();

    let target = build_target(&args)?;
    run(&args, &target)?;

    Ok(())
}

fn cargo(args: &Args, json: bool) -> Command {
    let mut cmd = Command::new("cargo");
    cmd.arg("build");

    if json {
        cmd.arg("--message-format=json");
    }
    if let Some(package) = &args.package {
        cmd.arg(format!("--package={package}"));
    }
    if let Some(bin) = &args.bin {
        cmd.arg(format!("--bin={bin}"));
    }
    if let Some(example) = &args.example {
        cmd.arg(format!("--example={example}"));
    }
    if args.tests {
        cmd.arg("--tests");
    }
    if let Some(test) = &args.test {
        cmd.arg(format!("--test={test}"));
    }
    if let Some(bench) = &args.bench {
        cmd.arg(format!("--bench={bench}"));
    }
    for feature in &args.features {
        cmd.arg("-F");
        cmd.arg(feature);
    }
    if args.all_features {
        cmd.arg("--all-features");
    }
    if args.no_default_features {
        cmd.arg("--no-default-features");
    }

    cmd
}

fn build_target(args: &Args) -> std::io::Result<PathBuf> {
    eprintln!("{:?}", cargo(args, true));
    let mut cmd = cargo(args, true).stdout(Stdio::piped()).spawn()?;

    macro_rules! bail {
        ($($tt:tt)*) => {{
            eprintln!($($tt)*);
            cmd.kill()?;
            exit(1);
        }};
    }

    let stdout = match cmd.stdout.take() {
        Some(stdout) => stdout,
        None => bail!("failed to capture stdout"),
    };

    let mut target: Option<PathBuf> = None;

    for line in BufReader::new(stdout).lines() {
        let line = line?;
        match serde_json::from_str(&line) {
            Ok(CargoMessage::CompilerArtifact {
                profile,
                executable,
            }) => {
                if let Some(executable) = executable {
                    if !profile.has_enough_debug_info() {
                        bail!(
                            "compiling without enough debug info (got {debug}); use at least `debug=1`",
                            debug = profile.debuginfo
                        );
                    }
                    if target.is_some() {
                        bail!("build produced more than one executable");
                    }
                    target = Some(executable);
                }
            }
            Ok(CargoMessage::BuildFinished { success }) => {
                if !success {
                    // run the build again to get error messages
                    cmd.kill()?;
                    return Err(cargo(args, false).exec());
                }
            }
            Ok(CargoMessage::Unknown) => {}
            Err(err) => bail!("failed to parse cargo output: {err:?}\noutput:\n{line}"),
        };
    }

    let Some(target) = target else {
        bail!("cargo did not output a compiler-artifact message");
    };

    Ok(target)
}

fn nnd(args: &Args, target: &Path) -> Command {
    let mut cmd = Command::new("nnd");

    if let Some(Breakpoint { file, line }) = &args.breakpoint {
        let file = match std::fs::canonicalize(Path::new(&file)) {
            Ok(file) => file,
            Err(err) => {
                eprintln!("{err}");
                exit(1);
            }
        };

        cmd.arg("--breakpoint");
        cmd.arg(format!("{file}:{line}", file = file.display()));
    } else {
        cmd.arg("-s");
    }
    cmd.arg(target);
    cmd.args(&args.extra_args);

    cmd
}

fn run(args: &Args, target: &Path) -> std::io::Result<()> {
    Err(nnd(args, target).exec())
}

/// Run a target built by cargo under `nnd`
#[derive(argh::FromArgs)]
struct Args {
    /// package to build
    #[argh(option, short = 'p')]
    package: Option<String>,

    /// build and debug the specified binary
    #[argh(option)]
    bin: Option<String>,

    /// build and debug the specified example
    #[argh(option)]
    example: Option<String>,

    /// build and debug tests
    #[argh(switch)]
    tests: bool,

    /// build and debug the specified test
    #[argh(option)]
    test: Option<String>,

    /// build and debug the specified bench
    #[argh(option)]
    bench: Option<String>,

    /// comma-separated list of features to activate
    #[argh(option, short = 'F')]
    features: Vec<String>,

    /// activate all available features
    #[argh(switch, long = "all-features")]
    all_features: bool,

    /// do not active the `default` feature
    #[argh(switch, long = "no-default-features")]
    no_default_features: bool,

    /// set a breakpoint (`file:line`)
    ///
    /// if not set, defaults to breakpoint on `main`
    #[argh(option, long = "breakpoint", short = 'b')]
    breakpoint: Option<Breakpoint>,

    /// extra arguments to pass to the built binary
    #[argh(positional)]
    extra_args: Vec<String>,
}

struct Breakpoint {
    file: String,
    line: usize,
}

impl argh::FromArgValue for Breakpoint {
    fn from_arg_value(value: &str) -> Result<Self, String> {
        let Some((file, line)) = value.split_once(':') else {
            return Err(format!("invalid breakpoint {value:?}, expected file:line"));
        };

        let file = file.to_owned();
        let line = line
            .parse()
            .map_err(|err| format!("invalid breakpoint line \"{line}\": {err}"))?;

        Ok(Self { file, line })
    }
}

#[derive(serde::Deserialize)]
#[serde(tag = "reason")]
enum CargoMessage {
    #[serde(rename = "compiler-artifact")]
    CompilerArtifact {
        profile: Profile,
        executable: Option<PathBuf>,
    },
    #[serde(rename = "build-finished")]
    BuildFinished { success: bool },

    #[serde(other)]
    Unknown,
}

#[derive(Debug, serde::Deserialize)]
struct Profile {
    debuginfo: DebugInfo,
}

impl Profile {
    fn has_enough_debug_info(&self) -> bool {
        match &self.debuginfo {
            DebugInfo::N(n) => match n {
                0 => false,
                1 | 2 => true,
                _ => panic!("invalid debuginfo value: {n}"),
            },

            DebugInfo::B(b) => match b {
                false => false,
                true => true,
            },

            DebugInfo::S(s) => match s.as_str() {
                "none" | "line-directives-only" | "line-tables-only" => false,
                "limited" | "full" => true,
                _ => panic!("invalid debuginfo value: {s}"),
            },
        }
    }
}

#[derive(Debug, serde::Deserialize, PartialEq, Eq, PartialOrd, Ord)]
#[serde(untagged)]
enum DebugInfo {
    N(usize),
    B(bool),
    S(String),
}

impl std::fmt::Display for DebugInfo {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            DebugInfo::N(v) => std::fmt::Display::fmt(v, f),
            DebugInfo::B(v) => std::fmt::Display::fmt(v, f),
            DebugInfo::S(v) => f.write_str(v),
        }
    }
}
