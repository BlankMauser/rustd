//! `cargo rustd build|run|test|check`: cargo's commands, with the daemon building what
//! changed since cargo last built. `CARGO.md` at the root of the repository is the design.

pub(crate) mod daemon;
pub(crate) mod wrapper;

mod cli;
mod client;
mod plan;
mod run;
mod say;
pub(crate) mod script;
mod state;
mod units;

use std::{path::Path, process::Command, time::Instant};

use anyhow::Context as _;

use cli::{Asked, Invocation, Subcommand};
use client::Start;
use daemon::{Checkout, Reply, Request};
use state::{Built, Counts, State};

/// `rustd cargo ...`: runs the command and ends with the status cargo would end with.
pub(crate) fn command(args: impl Iterator<Item = String>) -> anyhow::Result<()> {
    let code = match Asked::parse(args.collect()) {
        Asked::Build(asked) => match asked.only_cargo() {
            Some(reason) => {
                say::fallback(&reason, &format!("this is `cargo {}`", asked.subcommand.name()));
                let typed = std::iter::once(asked.subcommand.name().to_owned()).chain(asked.typed);
                plain_cargo(typed.collect())?
            }
            None => build(&asked)?,
        },
        Asked::Cargo(args) => plain_cargo(args)?,
        Asked::Status => status()?,
        Asked::Stop => stop()?,
    };
    std::process::exit(code)
}

/// Runs cargo with `args` and nothing of rustd's.
fn plain_cargo(args: Vec<String>) -> anyhow::Result<i32> {
    let status = Command::new(state::cargo_path()).args(args).status().context("running cargo")?;
    Ok(run::exit_code(status))
}

/// Builds what `asked` names, by the daemon when it can and by cargo otherwise, and then
/// runs what the subcommand runs.
fn build(asked: &Invocation) -> anyhow::Result<i32> {
    let started = Instant::now();
    let cwd = std::env::current_dir()?;
    let key = asked.key(&cwd);
    let known = State::read(&key);
    let state = match known.map(by_daemon) {
        Some(Ok(state)) => {
            let took = started.elapsed().as_secs_f64();
            let finished = format!("`{}` profile [rustd] target(s) in {took:.2}s", state.profile);
            say::status(asked.flags.quiet, "Finished", &finished);
            state
        }
        Some(Err((state, reason))) => {
            say::fallback(&reason, "building with cargo");
            let mut counts = state.counts;
            *counts.cargo.entry(reason).or_default() += 1;
            match by_cargo(asked, &cwd, &key, counts)? {
                Ok(state) => state,
                Err(code) => return Ok(code),
            }
        }
        None => match by_cargo(asked, &cwd, &key, Counts::default())? {
            Ok(state) => state,
            Err(code) => return Ok(code),
        },
    };
    state.write()?;
    after_build(asked, &state, &cwd)
}

/// Has the daemon bring the programs of `state` up to date and puts them where cargo has
/// them. `Err` is the state and why the build is cargo's.
fn by_daemon(mut state: State) -> Result<State, (State, String)> {
    let refused = state.refusal.clone().or_else(|| state.broken_guard());
    let refused = refused.or_else(|| {
        let mut asked = state.checkouts.iter().filter(|it| !it.programs.is_empty());
        asked.find_map(|checkout| synced(checkout).err())
    });
    let refused = refused.or_else(|| state.install().err().map(|it| format!("{it:#}")));
    match refused {
        Some(reason) => Err((state, reason)),
        None => {
            state.counts.served += 1;
            Ok(state)
        }
    }
}

/// Asks the daemon for the programs of `checkout`. `Err` is why it built none or not all.
fn synced(checkout: &Checkout) -> Result<(), String> {
    // The daemon knows the source files from when it was told to load them.
    let asked = Checkout { sources: Vec::new(), ..checkout.clone() };
    let reply = client::ask(Request::Sync(asked), Start::Never).map_err(|it| format!("{it:#}"))?;
    match reply {
        None => Err("no daemon is running".to_owned()),
        Some(Reply::Synced { programs }) => programs.into_iter().try_for_each(|it| it.map(drop)),
        Some(Reply::Cold { reason }) => Err(reason),
        Some(other) => Err(format!("the daemon answered {other:?}")),
    }
}

/// Tells the daemon to load `checkout` as cargo just built it, starting one when there is
/// none. `Err` is why it does not.
fn warmed(checkout: &Checkout) -> Result<(), String> {
    match client::ask(Request::Warm(checkout.clone()), Start::IfMissing) {
        Ok(Some(Reply::Cold { reason })) => Err(reason),
        Ok(_) => Ok(()),
        Err(why) => Err(format!("{why:#}")),
    }
}

/// Builds with cargo, and has the daemon load what cargo built for the builds to come.
/// `Err` is cargo's exit status when it failed.
fn by_cargo(
    asked: &Invocation,
    cwd: &Path,
    key: &str,
    counts: Counts,
) -> anyhow::Result<Result<State, i32>> {
    State::give_back(key);
    let built = match Built::by_cargo(asked)? {
        Ok(built) => built,
        Err(code) => return Ok(Err(code)),
    };
    let mut state = State::after(asked, cwd, key, built, counts)?;
    if state.refusal.is_none() {
        let asked = state.checkouts.iter().filter(|it| !it.programs.is_empty());
        let cold: Vec<String> = asked.filter_map(|it| warmed(it).err()).collect();
        for why in cold {
            say::fallback(&why, "the daemon has no part in the next build");
            state.count_cargo(&why);
        }
    }
    Ok(Ok(state))
}

/// What the subcommand does once the programs are built.
fn after_build(asked: &Invocation, state: &State, cwd: &Path) -> anyhow::Result<i32> {
    match asked.subcommand {
        Subcommand::Build => Ok(0),
        Subcommand::Run => match run::program(asked, state, cwd)? {
            Some(code) => Ok(code),
            // Cargo says which programs there are to pick from.
            None => {
                plain_cargo(std::iter::once("run".to_owned()).chain(asked.typed.clone()).collect())
            }
        },
        Subcommand::Test if asked.flags.no_run => Ok(0),
        Subcommand::Test => {
            let code = run::tests(asked, state, cwd)?;
            let stopped = code != 0 && !asked.flags.no_fail_fast;
            // Cargo 1.99 runs no doc tests when tests are picked by target or by name.
            let picked = asked.flags.selects_targets || !asked.positional.is_empty();
            if stopped || picked || !state.doc_tests {
                return Ok(code);
            }
            say::fallback("doc tests are built by rustdoc", "running `cargo test --doc`");
            Ok(code.max(run::doc_tests(asked)?))
        }
    }
}

/// `cargo rustd status`: the daemon, its workers, and how the builds of each checkout went.
fn status() -> anyhow::Result<i32> {
    match client::ask(Request::Status, Start::Never)? {
        Some(Reply::Status(daemon)) => {
            let idle =
                daemon.idle_secs.map_or("never".to_owned(), |it| format!("after {it}s idle"));
            println!("daemon: pid {}, version {}, exits {idle}", daemon.pid, daemon.version);
            for worker in daemon.workers {
                println!(
                    "  worker: pid {}, {}, log {}",
                    worker.pid,
                    worker.project.display(),
                    worker.log.display()
                );
                for (root, warmth) in worker.checkouts {
                    println!("    {}: {warmth}", root.display());
                }
            }
        }
        _ => println!("daemon: not running"),
    }
    for state in State::all() {
        let to_cargo: u64 = state.counts.cargo.values().sum();
        println!(
            "{} ({}, {}): {} builds by the daemon, {to_cargo} by cargo",
            state.root.display(),
            state.profile,
            state.key,
            state.counts.served
        );
        for (reason, count) in &state.counts.cargo {
            println!("  {count} x {reason}");
        }
        if let Some(refusal) = &state.refusal {
            println!("  every build is cargo's: {refusal}");
        }
    }
    Ok(0)
}

/// `cargo rustd stop`: has the daemon stop its workers and exit.
fn stop() -> anyhow::Result<i32> {
    match client::ask(Request::Stop, Start::Never)? {
        Some(_) => println!("daemon: stopping"),
        None => println!("daemon: not running"),
    }
    Ok(0)
}
