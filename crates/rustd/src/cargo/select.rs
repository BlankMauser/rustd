//! Running only the tests an edit can have changed the result of. The worker says, for
//! each test of a program it built, a print of all the code the test can run; a test
//! that passed with the same print, among the same tests and with the same things outside
//! the code, is said to have passed and is not run. `docs/incremental-tests.md` is the
//! design, and says what a print does not cover.

use std::{
    collections::{BTreeMap, BTreeSet},
    io::{BufRead as _, BufReader},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    time::{Instant, UNIX_EPOCH},
};

use anyhow::Context as _;
use serde_derive::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};

use super::{
    cli::Invocation,
    plan::Plan,
    state::{Program, State, as_built},
};

/// The variable that, set to `0`, has every test run: as `--rerun-all` does.
const SWITCH: &str = "RUSTD_TEST_CACHE";

/// The variables that differ between two calls from one shell and that no test is known to
/// read.
const UNREAD_ENV: &[&str] = &["_", "SHLVL", "OLDPWD", "RUSTD_STATS", SWITCH];

/// The flags of the test harness that leave what a test does, and how its result is
/// printed, as they are, each with whether a value follows.
const KNOWN_FLAGS: &[(&str, bool)] = &[
    ("--exact", false),
    ("--ignored", false),
    ("--include-ignored", false),
    ("--skip", true),
    ("--test-threads", true),
];

/// The results of the tests of one program that ran and did not fail.
#[derive(Serialize, Deserialize, Default)]
struct Results {
    /// What the results were had with besides the code: see [`outside`].
    outside: String,
    tests: BTreeMap<String, Passed>,
}

#[derive(Serialize, Deserialize, PartialEq, Clone)]
struct Passed {
    /// The print of the code the test could run.
    print: String,
    /// A print of the tests it shares a writable static with that ran in its process.
    company: String,
    ignored: bool,
}

/// What the worker said of a test: the print of what it can run, when all of it is
/// known, and the group of tests it shares writable statics with.
struct Reach {
    print: Option<String>,
    group: usize,
}

/// A test program whose tests can be told apart by what they run.
pub(super) struct Selective<'a> {
    program: &'a Program,
    reached: BTreeMap<String, Reach>,
    outside: String,
    store: PathBuf,
    flags: Vec<&'a String>,
}

impl<'a> Selective<'a> {
    /// `None` when every test of `program` has to run: the daemon did not build it, the
    /// worker said nothing of this build, the run is asked for in a way that is not known
    /// to leave results alone, or another program of the build is one it can't vouch for.
    pub(super) fn of(
        asked: &'a Invocation,
        state: &'a State,
        program: &'a Program,
    ) -> Option<Self> {
        if asked.flags.rerun_all || std::env::var_os(SWITCH).is_some_and(|it| it == "0") {
            return None;
        }
        let flags = harness_flags(asked)?;
        let plan = current(state, program)?;
        let reached = reached(plan)?;
        let mut hash = Sha256::new();
        let mut put = |text: &str| {
            hash.update(text.len().to_le_bytes());
            hash.update(text);
        };
        put(env!("CARGO_PKG_VERSION"));
        let (checkout, _) = program.asked_as?;
        put(&state.checkouts[checkout].generation.to_string());
        put(&plan.stamp.to_string());
        // A test can run any program of the build: the code of each is part of the result.
        for other in state.programs.iter().filter(|it| !it.test) {
            put(&other.executable.to_string_lossy());
            put(&whole_program(current(state, other)?)?);
        }
        for (name, value) in std::env::vars_os() {
            if !UNREAD_ENV.iter().any(|it| name == *it) {
                put(&name.to_string_lossy());
                put(&value.to_string_lossy());
            }
        }
        for (name, value) in &program.env {
            put(name);
            put(value);
        }
        flags.iter().filter(|it| it.contains("ignored")).for_each(|it| put(it));
        let compiled: BTreeSet<&Path> = state.sources().collect();
        let mut files = Vec::new();
        for dir in BTreeSet::from([state.root.as_path(), program.package_dir.as_path()]) {
            stamps(dir, &compiled, &mut files).ok()?;
        }
        files.sort_unstable();
        files.dedup();
        files.iter().for_each(|it| put(it));
        let outside = hex(&hash.finalize());
        let store = with_suffix(&plan.object, ".results");
        Some(Selective { program, reached, outside, store, flags })
    }

    /// Runs the tests `asked` names whose result is not known and says the result of the
    /// others. `Ok(None)` when the tests can't be listed, and so all have to run.
    pub(super) fn run(&self, asked: &Invocation) -> anyhow::Result<Option<ExitStatus>> {
        let started = Instant::now();
        let Some(named) = self.listed(asked)? else { return Ok(None) };
        let mut results = self.results();
        let company = self.company(&named);
        let known = |name: &String| {
            let reach = self.reached.get(name)?;
            let passed = results.tests.get(name)?;
            let same = Some(&passed.print) == reach.print.as_ref()
                && passed.company == company[&reach.group];
            same.then_some(passed.ignored)
        };
        // One test of a group that has to run has all of them run: they ran together.
        let stale: BTreeSet<usize> = named
            .iter()
            .filter(|it| known(it).is_none())
            .filter_map(|it| Some(self.reached.get(it)?.group))
            .collect();
        let cached = |name: &String| {
            let group = self.reached.get(name)?.group;
            known(name).filter(|_| !stale.contains(&group))
        };
        let (kept, run): (Vec<&String>, Vec<&String>) =
            named.iter().partition(|it| cached(it).is_some());
        println!("\nrunning {} tests", named.len());
        let (mut passed, mut ignored) = (0, 0);
        for name in &kept {
            match cached(name) {
                Some(true) => {
                    ignored += 1;
                    println!("test {name} ... ignored (cached)");
                }
                _ => {
                    passed += 1;
                    println!("test {name} ... ok (cached)");
                }
            }
        }
        let cached = Tally { passed, ignored, all: self.reached.len() };
        if run.is_empty() {
            let none = "test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out";
            let took = format!("finished in {:.2}s", started.elapsed().as_secs_f64());
            let line = cached.rewritten(&format!("{none}; {took}")).unwrap_or_default();
            println!("\n{line}\n");
            return Ok(Some(ExitStatus::default()));
        }
        let mut command = self.command()?;
        command.args(&self.flags);
        match kept.is_empty() {
            true => command.args(asked.positional.iter()),
            false => command.arg("--exact").args(&run),
        };
        command.stdout(Stdio::piped());
        let mut child = command.spawn().context("running the tests")?;
        let lines = BufReader::new(child.stdout.take().context("the tests have no output")?);
        let mut seen: BTreeMap<String, Option<bool>> = BTreeMap::new();
        for line in lines.lines() {
            let line = line?;
            if let Some((name, outcome)) = outcome(&line) {
                seen.insert(name.to_owned(), outcome);
            }
            match cached.rewritten(&line) {
                Some(line) => println!("{line}"),
                None if line.starts_with("running ") && line.ends_with(" tests") => {}
                None if line.starts_with("running ") && line.ends_with(" test") => {}
                None => println!("{line}"),
            }
        }
        let status = child.wait()?;
        for name in &run {
            let entry = seen.get(name.as_str()).copied().flatten().and_then(|ignored| {
                let reach = self.reached.get(*name)?;
                let (print, company) = (reach.print.clone()?, company[&reach.group].clone());
                Some(Passed { print, company, ignored })
            });
            match entry {
                Some(entry) => results.tests.insert((*name).clone(), entry),
                None => results.tests.remove(*name),
            };
        }
        results.outside.clone_from(&self.outside);
        std::fs::write(&self.store, serde_json::to_vec(&results)?)?;
        Ok(Some(status))
    }

    fn command(&self) -> std::io::Result<Command> {
        let mut command = Command::new(&self.program.executable);
        command.envs(self.program.env.iter().cloned()).current_dir(&self.program.package_dir);
        as_built(&mut command)?;
        Ok(command)
    }

    /// The tests the harness runs for `asked`, as it names them. `None` when it does not
    /// say, or names one the worker did not: its list is then not the worker's.
    fn listed(&self, asked: &Invocation) -> anyhow::Result<Option<Vec<String>>> {
        let mut command = self.command()?;
        command.args(["--list", "--format", "terse"]).args(asked.program_args());
        let listed = command.output().context("listing the tests")?;
        if !listed.status.success() {
            return Ok(None);
        }
        let text = String::from_utf8_lossy(&listed.stdout);
        let named: Option<Vec<String>> = text
            .lines()
            .map(|it| it.strip_suffix(": test").map(str::to_owned))
            .collect::<Option<Vec<_>>>()
            .filter(|named| named.iter().all(|it| self.reached.contains_key(it)));
        Ok(named)
    }

    /// The results of an earlier run that are still results: had with the same things
    /// outside the code.
    fn results(&self) -> Results {
        let read = std::fs::read(&self.store).ok();
        let results: Option<Results> = read.and_then(|it| serde_json::from_slice(&it).ok());
        results.filter(|it| it.outside == self.outside).unwrap_or_default()
    }

    /// For each group, a print of which of its tests are among `named`: what a test finds
    /// in a static is what the others that ran left there.
    fn company(&self, named: &[String]) -> BTreeMap<usize, String> {
        let mut hashes: BTreeMap<usize, Sha256> = BTreeMap::new();
        for name in named {
            let hash = hashes.entry(self.reached[name].group).or_default();
            hash.update(name.len().to_le_bytes());
            hash.update(name);
        }
        hashes.into_iter().map(|(group, hash)| (group, hex(&hash.finalize()))).collect()
    }
}

/// The counts of the tests that were not run, to add to the ones the harness prints.
struct Tally {
    passed: usize,
    ignored: usize,
    all: usize,
}

impl Tally {
    /// The harness's last line, `test result: ok. 3 passed; 0 failed; ...`, with the tests
    /// that were not run counted as what they are, and said to be.
    fn rewritten(&self, line: &str) -> Option<String> {
        let (verdict, counts) = line.strip_prefix("test result: ")?.split_once(". ")?;
        let mut parts = counts.split("; ");
        let mut count =
            |word: &str| -> Option<usize> { parts.next()?.strip_suffix(word)?.trim().parse().ok() };
        let passed = count("passed")? + self.passed;
        let failed = count("failed")?;
        let ignored = count("ignored")? + self.ignored;
        let measured = count("measured")?;
        let _ = count("filtered out")?;
        let rest = parts.next()?;
        let filtered = self.all.saturating_sub(passed + failed + ignored + measured);
        let cached = |count: usize| match count {
            0 => String::new(),
            count => format!(" ({count} cached)"),
        };
        Some(format!(
            "test result: {verdict}. {passed} passed{}; {failed} failed; {ignored} ignored{}; \
             {measured} measured; {filtered} filtered out; {rest}",
            cached(self.passed),
            cached(self.ignored),
        ))
    }
}

/// The test a line of the harness is the result of, and whether it was ignored: `None`
/// for one that did not pass.
fn outcome(line: &str) -> Option<(&str, Option<bool>)> {
    let (name, result) = line.strip_prefix("test ")?.rsplit_once(" ... ")?;
    let name = name.strip_suffix(" - should panic").unwrap_or(name);
    let outcome = match result {
        "ok" => Some(false),
        _ if result == "ignored" || result.starts_with("ignored, ") => Some(true),
        _ => None,
    };
    Some((name, outcome))
}

/// The flags `asked` gives the harness, when each is one of [`KNOWN_FLAGS`].
fn harness_flags(asked: &Invocation) -> Option<Vec<&String>> {
    let mut flags = Vec::new();
    let mut words = asked.trailing.iter();
    while let Some(word) = words.next() {
        if !word.starts_with('-') {
            return None;
        }
        let name = word.split_once('=').map_or(word.as_str(), |it| it.0);
        let (_, valued) = KNOWN_FLAGS.iter().find(|it| it.0 == name)?;
        let value = (*valued && !word.contains('=')).then(|| words.next());
        // What is skipped and what is named exactly are in the list of tests to run.
        if !matches!(name, "--skip" | "--exact") {
            flags.push(word);
            flags.extend(value.flatten());
        }
    }
    Some(flags)
}

/// The plan of `program` when the executable cargo has for it is the one the daemon
/// built from it.
fn current<'a>(state: &'a State, program: &Program) -> Option<&'a Plan> {
    let (checkout, at) = program.asked_as?;
    let plan = state.checkouts.get(checkout)?.programs.get(at)?;
    same_file::is_same_file(&plan.binary, &program.executable).ok()?.then_some(plan)
}

fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// The lines the worker wrote of the program of `plan`, when it wrote them
/// after it last wrote the program: a build it said nothing of leaves older ones.
fn lines(plan: &Plan) -> Option<String> {
    let path = with_suffix(&plan.object, ".reached");
    let written = |path: &Path| std::fs::metadata(path).and_then(|it| it.modified()).ok();
    (written(&path)? >= written(&plan.binary)?).then(|| std::fs::read_to_string(path).ok())?
}

fn reached(plan: &Plan) -> Option<BTreeMap<String, Reach>> {
    let lines = lines(plan)?;
    let tests = lines.lines().filter_map(|it| it.strip_prefix("test "));
    tests
        .map(|it| {
            let mut fields = it.splitn(3, ' ');
            let print = fields.next().filter(|it| *it != "-").map(str::to_owned);
            let group = fields.next()?.parse().ok()?;
            Some((fields.next()?.to_owned(), Reach { print, group }))
        })
        .collect()
}

/// The print of all a program that is not a test can run, when it is known.
fn whole_program(plan: &Plan) -> Option<String> {
    let lines = lines(plan)?;
    let print = lines.lines().find_map(|it| it.strip_prefix("program "))?;
    (print != "-").then(|| print.to_owned())
}

/// Adds what says whether each file under `dir` is as it was: its length and when it was
/// written. Left out are the files in `compiled`, which the prints are of, and what git
/// and the builds write.
fn stamps(dir: &Path, compiled: &BTreeSet<&Path>, files: &mut Vec<String>) -> std::io::Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let kind = entry.file_type()?;
        if kind.is_dir() {
            let built = path.join("CACHEDIR.TAG").exists();
            if !built && entry.file_name() != ".git" {
                stamps(&path, compiled, files)?;
            }
            continue;
        }
        if compiled.contains(path.as_path()) {
            continue;
        }
        // A link is the file it names.
        let Ok(file) = std::fs::metadata(&path) else {
            files.push(format!("{} gone", path.display()));
            continue;
        };
        let written = file.modified()?.duration_since(UNIX_EPOCH).unwrap_or_default();
        files.push(format!("{} {} {}", path.display(), file.len(), written.as_nanos()));
    }
    Ok(())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|it| format!("{it:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_harness() {
        assert_eq!(outcome("test a::b ... ok"), Some(("a::b", Some(false))));
        assert_eq!(outcome("test a::b ... ignored, slow"), Some(("a::b", Some(true))));
        assert_eq!(outcome("test a - should panic ... ok"), Some(("a", Some(false))));
        assert_eq!(outcome("test a::b ... FAILED"), Some(("a::b", None)));
        assert_eq!(outcome("test result: ok. 1 passed"), None);
    }

    #[test]
    fn counts_what_was_not_run() {
        let tally = Tally { passed: 5, ignored: 1, all: 10 };
        let line = "test result: FAILED. 1 passed; 1 failed; 0 ignored; 0 measured; 8 filtered \
                    out; finished in 0.01s";
        assert_eq!(
            tally.rewritten(line).as_deref(),
            Some(
                "test result: FAILED. 6 passed (5 cached); 1 failed; 1 ignored (1 cached); 0 \
                 measured; 2 filtered out; finished in 0.01s"
            )
        );
    }
}
