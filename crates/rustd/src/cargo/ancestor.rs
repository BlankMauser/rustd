//! The cargo that runs the wrapper. Cargo tells the rustc it runs nothing of its own command
//! line, and which build a unit is of is read from it: the command, its flags, the directory
//! it runs in and its environment.

use std::{ffi::OsString, path::PathBuf};

/// A cargo that is running, as the system describes it.
pub(crate) struct Cargo {
    pub(crate) pid: u32,
    pub(crate) executable: PathBuf,
    /// Its arguments, without its own name.
    pub(crate) args: Vec<String>,
    pub(crate) cwd: PathBuf,
    pub(crate) env: Vec<(OsString, OsString)>,
}

/// How many processes up from the wrapper cargo is looked for: there are the wrappers the
/// user chains between the two, and nothing else.
const FARTHEST: usize = 8;

impl Cargo {
    /// The nearest process above this one that is cargo. `Err` is why none is known.
    pub(crate) fn above() -> Result<Cargo, String> {
        let mut pid = std::process::id();
        for _ in 0..FARTHEST {
            pid = system::parent(pid).ok_or("the process that runs rustc is not cargo")?;
            if let Some(cargo) = system::described(pid).filter(Cargo::is_cargo) {
                return Ok(cargo);
            }
        }
        Err("the process that runs rustc is not cargo".to_owned())
    }

    fn is_cargo(&self) -> bool {
        self.executable.file_stem().is_some_and(|it| it == "cargo")
    }
}

#[cfg(target_os = "linux")]
mod system {
    use std::{
        ffi::OsString,
        os::unix::ffi::OsStringExt as _,
        path::{Path, PathBuf},
    };

    use super::Cargo;

    fn of(pid: u32, file: &str) -> PathBuf {
        Path::new("/proc").join(pid.to_string()).join(file)
    }

    /// The process that started `pid`, when `pid` runs.
    pub(super) fn parent(pid: u32) -> Option<u32> {
        let stat = std::fs::read_to_string(of(pid, "stat")).ok()?;
        // `pid (name) state ppid ...`, and the name may have spaces and brackets of its own.
        let after_name = &stat[stat.rfind(')')? + 1..];
        let mut fields = after_name.split_whitespace();
        let state = fields.next()?;
        let parent = fields.next()?.parse().ok()?;
        // A process that ended and is not reaped yet.
        (state != "Z").then_some(parent)
    }

    fn words(pid: u32, file: &str) -> Option<Vec<OsString>> {
        let bytes = std::fs::read(of(pid, file)).ok()?;
        let words = bytes.split(|it| *it == 0).filter(|it| !it.is_empty());
        Some(words.map(|it| OsString::from_vec(it.to_vec())).collect())
    }

    pub(super) fn described(pid: u32) -> Option<Cargo> {
        let args = words(pid, "cmdline")?.into_iter().skip(1);
        let env = words(pid, "environ")?.into_iter().filter_map(|it| {
            let bytes = it.into_vec();
            let at = bytes.iter().position(|it| *it == b'=')?;
            let (name, value) = (bytes[..at].to_vec(), bytes[at + 1..].to_vec());
            Some((OsString::from_vec(name), OsString::from_vec(value)))
        });
        Some(Cargo {
            pid,
            executable: std::fs::read_link(of(pid, "exe")).ok()?,
            args: args.map(|it| it.into_string().ok()).collect::<Option<_>>()?,
            cwd: std::fs::read_link(of(pid, "cwd")).ok()?,
            env: env.collect(),
        })
    }
}

/// On Windows a process's parent, command line, directory and environment are in its own
/// memory, in the parameters its environment block points to, and are read from there. The
/// places are those of a 64-bit process, which is all rustd is built as there.
#[cfg(all(windows, target_pointer_width = "64"))]
mod system {
    use std::{
        ffi::{OsString, c_void},
        os::windows::{
            ffi::OsStringExt as _,
            io::{AsRawHandle as _, FromRawHandle as _, OwnedHandle},
        },
        path::PathBuf,
        ptr,
    };

    use windows_sys::Win32::{
        Foundation::{FILETIME, LocalFree},
        System::{
            Diagnostics::Debug::ReadProcessMemory,
            Threading::{
                GetProcessTimes, IsWow64Process, OpenProcess, PROCESS_QUERY_INFORMATION,
                PROCESS_VM_READ, QueryFullProcessImageNameW,
            },
        },
        UI::Shell::CommandLineToArgvW,
    };

    use super::Cargo;

    #[link(name = "ntdll")]
    unsafe extern "system" {
        fn NtQueryInformationProcess(
            process: *mut c_void,
            class: u32,
            information: *mut c_void,
            len: u32,
            returned: *mut u32,
        ) -> i32;
    }

    /// Where a process's environment block has its parameters, and where those have the
    /// directory, the command line, the environment and how long that is.
    const PARAMETERS: usize = 0x20;
    const DIRECTORY: usize = 0x38;
    const COMMAND_LINE: usize = 0x70;
    const ENVIRONMENT: usize = 0x80;
    const ENVIRONMENT_SIZE: usize = 0x3f0;

    /// An environment longer than this is not one: Windows allows 32767 characters.
    const LONGEST: usize = 1 << 20;

    struct Process(OwnedHandle);

    impl Process {
        fn open(pid: u32) -> Option<Process> {
            // SAFETY: opens a process by number; the handle is owned from here.
            let handle =
                unsafe { OpenProcess(PROCESS_QUERY_INFORMATION | PROCESS_VM_READ, 0, pid) };
            if handle.is_null() {
                return None;
            }
            // SAFETY: the handle was just opened and is nobody else's.
            let process = Process(unsafe { OwnedHandle::from_raw_handle(handle) });
            let mut narrow = 0;
            // SAFETY: the handle is open and the result is written to a local.
            let asked = unsafe { IsWow64Process(process.0.as_raw_handle(), &raw mut narrow) };
            (asked != 0 && narrow == 0).then_some(process)
        }

        /// `ProcessBasicInformation`: the second word is where the environment block is and
        /// the sixth is the process that started this one.
        fn basic(&self) -> Option<[usize; 6]> {
            let mut basic = [0usize; 6];
            // SAFETY: the buffer is as long as is said and is the size of the structure.
            let status = unsafe {
                NtQueryInformationProcess(
                    self.0.as_raw_handle(),
                    0,
                    basic.as_mut_ptr().cast(),
                    size_of_val(&basic) as u32,
                    ptr::null_mut(),
                )
            };
            (status >= 0).then_some(basic)
        }

        /// When the process was started.
        fn started(&self) -> Option<u64> {
            let mut times = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
            let [created, exited, kernel, user] = &mut times;
            // SAFETY: the handle is open and the four times are written to locals.
            let asked =
                unsafe { GetProcessTimes(self.0.as_raw_handle(), created, exited, kernel, user) };
            (asked != 0)
                .then(|| u64::from(created.dwHighDateTime) << 32 | u64::from(created.dwLowDateTime))
        }

        fn read<T: Copy + Default>(&self, at: usize, len: usize) -> Option<Vec<T>> {
            let mut read = vec![T::default(); len];
            let (bytes, mut done) = (size_of_val(&read[..]), 0);
            // SAFETY: the buffer is `bytes` long; a place that is not the process's fails.
            let ok = unsafe {
                ReadProcessMemory(
                    self.0.as_raw_handle(),
                    at as *const c_void,
                    read.as_mut_ptr().cast(),
                    bytes,
                    &raw mut done,
                )
            };
            (ok != 0 && done == bytes).then_some(read)
        }

        fn word(&self, at: usize) -> Option<usize> {
            self.read::<usize>(at, 1)?.pop()
        }

        /// The text of the `UNICODE_STRING` at `at`: its length in bytes, and where it is.
        fn text(&self, at: usize) -> Option<Vec<u16>> {
            let len = self.word(at)? & 0xffff;
            self.read(self.word(at + 8)?, len / 2)
        }
    }

    /// The process that started `pid`, when it still runs. The number of one that ended may
    /// be another's by now, which is known by its having started later.
    pub(super) fn parent(pid: u32) -> Option<u32> {
        let process = Process::open(pid)?;
        let parent = u32::try_from(process.basic()?[5]).ok()?;
        (Process::open(parent)?.started()? <= process.started()?).then_some(parent)
    }

    pub(super) fn described(pid: u32) -> Option<Cargo> {
        let process = Process::open(pid)?;
        let parameters = process.word(process.basic()?[1] + PARAMETERS)?;
        let mut cwd = process.text(parameters + DIRECTORY)?;
        // `C:\` is a directory, and any other is named without the `\` it is kept with.
        if cwd.len() > 3 && cwd.last() == Some(&u16::from(b'\\')) {
            cwd.pop();
        }
        let size = process.word(parameters + ENVIRONMENT_SIZE)?;
        let environment = process.word(parameters + ENVIRONMENT)?;
        let environment = process.read::<u16>(environment, size.min(LONGEST) / 2)?;
        let env = environment.split(|it| *it == 0).filter_map(|it| {
            // The directory of each drive is kept as a variable whose name begins with `=`.
            let at = it.iter().skip(1).position(|it| *it == u16::from(b'='))? + 1;
            (it[0] != u16::from(b'=')).then(|| (wide(&it[..at]), wide(&it[at + 1..])))
        });
        Some(Cargo {
            pid,
            executable: executable(&process)?,
            args: arguments(process.text(parameters + COMMAND_LINE)?)?,
            cwd: PathBuf::from(wide(&cwd)),
            env: env.collect(),
        })
    }

    fn wide(text: &[u16]) -> OsString {
        OsString::from_wide(text)
    }

    fn executable(process: &Process) -> Option<PathBuf> {
        let mut name = vec![0u16; 1 << 15];
        let mut len = name.len() as u32;
        // SAFETY: the buffer is `len` characters long, and `len` is set to what was written.
        let asked = unsafe {
            QueryFullProcessImageNameW(
                process.0.as_raw_handle(),
                0,
                name.as_mut_ptr(),
                &raw mut len,
            )
        };
        (asked != 0).then(|| PathBuf::from(wide(&name[..len as usize])))
    }

    /// The arguments of a command line as the C runtime takes it apart, without the first.
    fn arguments(mut line: Vec<u16>) -> Option<Vec<String>> {
        line.push(0);
        let mut count = 0;
        // SAFETY: the line ends with a zero. What is returned is `count` pointers to texts
        // that end with one, in one allocation that is freed below.
        let words = unsafe { CommandLineToArgvW(line.as_ptr(), &raw mut count) };
        if words.is_null() {
            return None;
        }
        let word = |at: usize| {
            // SAFETY: `at` is below `count`, and the text ends with a zero.
            let text = unsafe {
                let text = *words.add(at);
                std::slice::from_raw_parts(text, (0..).take_while(|it| *text.add(*it) != 0).count())
            };
            wide(text).into_string().ok()
        };
        let args = (1..count as usize).map(word).collect();
        // SAFETY: the allocation `CommandLineToArgvW` made, which nothing points into now.
        unsafe { LocalFree(words.cast()) };
        args
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn a_process_is_described_as_it_was_started() {
            let mut child = std::process::Command::new("cmd");
            child.args(["/c", "pause", "a b", "c\"d"]).env("RUSTD_ANCESTOR", "one=two");
            child.current_dir(std::env::temp_dir()).stdin(std::process::Stdio::piped());
            let mut child = child.stdout(std::process::Stdio::null()).spawn().unwrap();
            let seen = described(child.id());
            let above = parent(child.id());
            child.kill().unwrap();
            child.wait().unwrap();
            let seen = seen.unwrap();
            assert_eq!(above, Some(std::process::id()));
            assert_eq!(seen.executable.file_stem().unwrap().to_ascii_lowercase(), "cmd");
            assert_eq!(seen.args, ["/c", "pause", "a b", "c\"d"]);
            let temp = std::env::temp_dir().canonicalize().unwrap();
            assert_eq!(seen.cwd.canonicalize().unwrap(), temp);
            let named = seen.env.iter().find(|(name, _)| name == "RUSTD_ANCESTOR");
            assert_eq!(named.map(|it| it.1.to_str().unwrap()), Some("one=two"));
            assert!(seen.env.iter().all(|(name, _)| !name.is_empty()));
        }
    }
}

/// Elsewhere the command line of another process is not read yet, and every build under
/// plain cargo is rustc's.
#[cfg(not(any(target_os = "linux", all(windows, target_pointer_width = "64"))))]
mod system {
    use super::Cargo;

    pub(super) fn parent(_pid: u32) -> Option<u32> {
        None
    }

    pub(super) fn described(_pid: u32) -> Option<Cargo> {
        None
    }
}
