# Security

mossbed is a thin supervisor around
[tardigrade](https://github.com/zbrdc/tardigrade): the containment is
tardigrade's, and this server decides whether to use it, how long a run may
last, how much memory and filesystem it may take, and which named network
endpoints — if any — the operator has granted it.

## Reporting a vulnerability

Open a private security advisory on this repository rather than a public issue
for anything exploitable. This is a personal project with no service-level
promise; expect a reply when there is one.

## What this server is responsible for

- **Fail-closed startup.** If `tardigrade --sandbox-check` does not succeed,
  every run is refused and the server stays up. It never falls back to running
  unconfined, and there is no flag that makes it. The same gate probes the
  flags the binary advertises and refuses, by name, a binary too old for the
  flags this runner passes (or for configured network grants).
- **Bounding each run.** A parent-enforced wall-clock deadline (cancellable
  by the client), an address-space belt through `prlimit`, an explicit heap
  cap and RAM-filesystem quota handed to tardigrade, and size caps on inbound
  code/snapshots as well as outbound output/dumps. Every ceiling that bounds
  the guest is an operator setting, never a tool argument.
  The belt is loose by necessity — it has to cover V8's own address-space
  reservation, tens of GiB in the current build — so the heap cap is what
  bounds a guest; the belt only backstops the process as a whole.
- **Keeping the guest offline by default.** Network access exists only when
  the operator grants named endpoints (`MOSSBED_ALLOW_CALL`). The
  guest names a capability; it never sees a URL, cannot name an ungranted
  endpoint, and private/loopback resolution stays refused unless the operator
  explicitly opts in. Grants, budgets and address enforcement otherwise live
  in tardigrade's sidecar.
- **Bounding sessions.** A `--serve` session is one guest under the same
  cage and caps as a one-shot run, plus a mandatory per-command timeout, a
  whole-session TTL, and operator caps on session count and idle time.
  Session state lives only in the guest's RAM filesystem and memory.
- **Handling guest scripts privately.** A 0700 per-process directory, a 0600
  file created with `O_EXCL` so a planted symlink cannot redirect the write,
  removed when the run or session ends.
- **Reporting honestly.** Every result states the clock, the caps actually
  applied, and the names (never URLs) of any granted network capabilities.
  Recognised guest failures are explained above the original trace rather
  than passed through bare, with the cap values actually in force.

## What it is not responsible for

- **The containment itself.** Landlock, seccomp, the RAM filesystem and the V8
  isolate are tardigrade's. Its `docs/SANDBOX.md` and `docs/KNOWN_RISKS.md`
  are authoritative, and its `docs/KNOWN_BUGS.md` lists what is currently
  broken — notably that Intl is unavailable without ICU data.
- **The content of granted endpoints.** A grant is a narrow pipe to one
  operator-declared URL; what that endpoint does with a guest-supplied body
  is the operator's decision to have made.
- **Anything past the process boundary.** This server speaks stdio to one
  caller, with no authentication and no transport of its own. Do not expose it
  over a network, and treat the caller as trusted to run code in the sandbox.

## Tests

The suites that verify the boundary need the real tardigrade binary. Without it
they skip and report success, which is why `TARDIGRADE_REQUIRE_TESTS=1` exists:
set it alongside `TARDIGRADE_BIN` and a missing binary fails the run, which is
what a release check should do.
