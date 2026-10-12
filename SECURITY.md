# Security

tardigrade-runner is a thin supervisor around
[tardigrade](https://github.com/zbrdc/tardigrade): the containment is
tardigrade's, and this server decides whether to use it, how long a run may
last and how much memory it may take.

## Reporting a vulnerability

Open a private security advisory on this repository rather than a public issue
for anything exploitable. This is a personal project with no service-level
promise; expect a reply when there is one.

## What this server is responsible for

- **Fail-closed startup.** If `tardigrade --sandbox-check` does not succeed,
  every run is refused and the server stays up. It never falls back to running
  unconfined, and there is no flag that makes it.
- **Bounding each run.** A parent-enforced wall-clock deadline, an address-space
  belt through `prlimit`, and an explicit heap cap handed to tardigrade.
- **Handling guest scripts privately.** A 0700 per-process directory, a 0600
  file created with `O_EXCL` so a planted symlink cannot redirect the write,
  removed when the run ends.
- **Reporting honestly.** Every result states the clock and the caps actually
  applied, and a recognised V8 abort is explained above the original trace
  rather than passed through bare.

## What it is not responsible for

- **The containment itself.** Landlock, seccomp, the RAM filesystem and the V8
  isolate are tardigrade's. Its `SANDBOX.md` and `KNOWN_RISKS.md` are
  authoritative, and its `KNOWN_BUGS.md` lists what is currently broken -
  notably that the Intl date APIs abort the guest.
- **Anything past the process boundary.** This server speaks stdio to one
  caller, with no authentication and no transport of its own. Do not expose it
  over a network, and treat the caller as trusted to run code in the sandbox.

## Tests

The suites that verify the boundary need the real tardigrade binary. Without it
they skip and report success, which is why `TARDIGRADE_REQUIRE_TESTS=1` exists:
set it alongside `TARDIGRADE_BIN` and a missing binary fails the run, which is
what a release check should do.

