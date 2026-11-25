# sqlTrace

English | [日本語](README.ja.md)

sqlTrace shows the SQL that a Windows application sends to its database, without changing the application.
It attaches to the target process as a debugger and reads the SQL at the entry points of the database client
APIs (ODBC, Oracle OCI and others). It does not inject DLLs, and it does not replace drivers or put a proxy in
between. When you stop it, it removes its breakpoints and detaches, and the target keeps running.

It is useful for looking into legacy line-of-business applications without source code, or any application
where you do not know which SQL is issued where.

## Features

- Captured APIs (layers)

  | Layer | DLL | Functions |
  |---|---|---|
  | `odbc` | `odbc32.dll` | `SQLExecDirect(A/W)`, `SQLPrepare(A/W)`, `SQLExecute` |
  | `oci` | `oci.dll` | `OCIStmtPrepare`, `OCIStmtPrepare2`, `OCIStmtExecute` |
  | `oci7` | `ociw32.dll`, `oci.dll` | `oparse`, `osql3`, `oexec`, `oexn`, `oexfet` |
  | `dblib` | `ntwdblib.dll` | `dbcmd`, `dbsqlexec`, `dbsqlsend` |

  Calls that an ODBC driver makes internally (for example to OCI) are not reported twice. Use `--keep-nested`
  to report them as well. Prepare and execute calls on the same handle are matched, so execute rows show the
  SQL too.
- Attach to a running process, or launch a program and trace it from the start.
- The x64 build handles both 64-bit and 32-bit (WOW64) processes.
- Outputs: console (with SQL syntax highlighting), HTML (a single self-contained file with a filter),
  Excel (xlsx), and NDJSON (the raw record, which can be exported to HTML or Excel later).
- Both a GUI (`sqltrace-gui.exe`) and a command line tool (`sqltrace.exe`).

## Download

Download the zip from [Releases](../../releases) and extract it anywhere. No installation is needed.

- `x64`: for 64-bit Windows (it also traces 32-bit applications).
- `x86`: for 32-bit Windows.

The user interface and messages are in Japanese for now.

## Usage (GUI)

Open `sqltrace-gui.exe`, choose the target in one of these ways, and press "▶ 開始" (Start).

- **Attach**: pick a process from the list, or press and hold "◎ 照準" (crosshair), drag it onto the target's
  window and release.
- **Launch**: enter the executable, arguments and working folder.

The captured SQL appears in the table. "実行だけ" (execute only) limits it to execute rows. Choose a
destination to write HTML, Excel or NDJSON. "■ 離れる" (detach) removes the breakpoints and detaches.

## Usage (command line)

```text
sqltrace attach <PID> [options]             attach to a running process
sqltrace run [options] -- <EXE> [args...]   launch a program and trace it from the start
sqltrace export <record.ndjson> [outputs]   export a saved record to HTML or Excel
sqltrace repair <PID>                       restore breakpoints left behind by a forced termination
sqltrace layers                             list the layers that can be captured
```

Examples:

```powershell
# Attach to PID 1234, show the SQL on screen, and also write HTML and Excel
sqltrace attach 1234 --html trace.html --xlsx trace.xlsx

# Launch with ODBC only, keep the raw record, and export it later
sqltrace run --layers odbc --ndjson trace.ndjson -- "C:\App\app.exe" /arg
sqltrace export trace.ndjson --html trace.html
```

Press Ctrl+C to stop. SQL goes to standard output and status and warnings go to standard error, so
`sqltrace attach 1234 > sql.txt` saves only the SQL to a file.

Run `sqltrace help` for all options.

## Notes

- You need permission to debug the target process. To attach to a process running as administrator, run
  sqltrace as administrator too.
- Only one debugger can be attached to a process. If another debugger such as Visual Studio is already
  attached, sqltrace cannot attach. A second sqltrace on the same process is refused.
- While tracing, the target pauses briefly at every database API call, so it runs a little slower.
- **If sqltrace is killed, its breakpoints stay in the target**, and the target crashes when it next calls the
  API. If that happens, run `sqltrace repair <PID>` right away. Stopping normally (Ctrl+C, "■ 離れる" or
  closing the window) removes the breakpoints properly.
- Version 0.1.0 reads only the SQL text. Return values, elapsed time and bind values are not captured yet.
- Pro*C, JDBC, and .NET providers that do not go through a native API (ODP.NET Managed, SqlClient and others)
  are not supported yet.

## Building

You need Rust (the version is pinned in `rust-toolchain.toml`) and the MSVC build tools.

```powershell
cargo build --release --target x86_64-pc-windows-msvc
```

## License

MIT (see [LICENSE](LICENSE))
