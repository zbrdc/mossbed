# Running other languages

Mossbed executes JavaScript and WebAssembly through tardigrade's V8 isolate.
The `code` and `entry_code` fields always contain JavaScript. They do not select
a compiler based on a filename or source language.

## JavaScript

Call `run_js` with these arguments:

```json
{"code":"console.log(6 * 7)"}
```

The result has `status: "ok"` and `stdout` containing `42`.
The guest provides `console`, timers, typed arrays, WebAssembly, and `vm.fs`.
It does not provide Node.js, a browser DOM, `require`, `process`, or `fetch`.

Classic scripts are the default. With a compatible engine, `module: true`
enables ES modules and top-level `await`. Imports resolve inside the RAM
filesystem. Bundle bare package imports before execution.

## TypeScript

Transpile TypeScript on the host before calling mossbed. For example, save this
as `app.ts`:

```ts
const answer: number = 6 * 7;
console.log(answer);
```

With Bun installed on the host, build one JavaScript bundle:

```sh
bun build app.ts --target=browser --format=iife --outfile app.js
```

Send the contents of `app.js` as `run_js.code`. The browser target avoids
Node-specific output. It does not add browser APIs to the guest. Bun does not
type-check the source. When type checking is needed, use a separate type checker.

An explicitly supplied WebAssembly compiler can run inside the guest, but
mossbed does not include one. The engine's
[in-guest TypeScript example](https://github.com/zbrdc/tardigrade/tree/main/examples/ts-in-guest)
is experimental and requires its own compiler artifact and WASI shim.

## C and C++

Use a compiler with a WebAssembly backend and linker, such as Clang with
`wasm-ld`. Save this freestanding C example as `add.c`:

```c
int add(int a, int b) { return a + b; }
```

Build it on the host:

```sh
clang --target=wasm32 -O2 -nostdlib -Wl,--no-entry -Wl,--export=add add.c -o add.wasm
```

For C++, save this as `add.cpp`:

```cpp
extern "C" int add(int a, int b) { return a + b; }
```

```sh
clang++ --target=wasm32 -O2 -nostdlib -Wl,--no-entry -Wl,--export=add add.cpp -o add.wasm
```

These examples need no libc or WASI imports. Programs that use standard
libraries or operating-system services need a compatible runtime and imports.

## Rust

Save this as `add.rs`:

```rust
#[no_mangle]
pub extern "C" fn add(a: i32, b: i32) -> i32 { a + b }
```

Install the target and compile on the host:

```sh
rustup target add wasm32-unknown-unknown
rustc --edition=2021 --target wasm32-unknown-unknown --crate-type cdylib -O add.rs -o add.wasm
```

Crates that require host threads, sockets, or filesystem access do not become
compatible merely by selecting a WebAssembly target.

## Load the WebAssembly module

For a compiled `add.wasm` file, encode its bytes on the host:

```sh
base64 -w0 add.wasm
```

Replace `BASE64_OF_ADD_WASM` in this `run_js` argument object with that output:

```json
{
  "preload": "{\"version\":2,\"dirs\":[\"/\"],\"files\":{\"/add.wasm\":{\"b64\":\"BASE64_OF_ADD_WASM\"}}}",
  "code": "const module = new WebAssembly.Module(vm.fs.readBytes('/add.wasm')); const instance = new WebAssembly.Instance(module, {}); console.log(instance.exports.add(20, 22));"
}
```

`preload` is a JSON **string**, not a nested object. The module lives in the
guest's RAM filesystem, not at a host path. Successful execution prints `42`.
Base64 and snapshot overhead count toward `MOSSBED_INPUT_MAX`.

The empty import object works for these examples. For other modules, inspect
`WebAssembly.Module.imports(module)` and supply every required import.
Mossbed does not provide WASI automatically. A WASI shim must implement the
module's expected interface using guest APIs such as `vm.fs`.

## Persistent sessions

When the engine supports `--serve`, call `session_open` with the same `preload`
and this JavaScript as `entry_code`:

```js
globalThis.add = new WebAssembly.Instance(
  new WebAssembly.Module(vm.fs.readBytes('/add.wasm')), {}
).exports.add;
```

Pass the returned session ID to `session_eval`, with `code: "add(20, 22)"`.
The reply has `ok: true` and `value: 42`. Close it with `session_close`.
Unlike `run_js`, a session retains the instantiated module between evaluations.

## Limits

- Other languages need a compatible JavaScript or WebAssembly compiler target.
  Interpreted languages can require a supplied interpreter as well.
- Native Linux executables, shell commands, and compiler processes cannot run
  inside the isolate. Host preparation is outside mossbed's sandbox.
- Compiled code still depends on the guest's available imports and APIs.
  Compilation does not grant host access or bypass the sandbox.
- `MOSSBED_MEM` is not a total-process memory limit. Module-defined WebAssembly
  memory needs particular care. See [Security](../SECURITY.md#language-and-build-boundaries).

The engine's [guest API](https://github.com/zbrdc/tardigrade/blob/main/docs/GUEST-API.md)
describes the available APIs. [README](../README.md) covers installation and operator limits.
