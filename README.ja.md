# sqlTrace

[English](README.md) | 日本語

Windows のアプリケーションが DB に送る SQL を、そのアプリケーションに手を加えずに見るための道具です。
対象のプロセスにデバッガとして付き、DB のクライアントの API（ODBC、Oracle OCI など）の入口で SQL を
読み取ります。DLL の注入も、ドライバーやプロキシーの入れ替えもしません。止めればブレークポイントを外して
離れ、対象はそのまま動き続けます。

ソースのない古い業務アプリや、どこで何の SQL を出しているか分からないアプリを調べるのに向いています。

## できること

- 捕まえる API（層）

  | 層 | DLL | 関数 |
  |---|---|---|
  | `odbc` | `odbc32.dll` | `SQLExecDirect(A/W)`、`SQLPrepare(A/W)`、`SQLExecute` |
  | `oci` | `oci.dll` | `OCIStmtPrepare`、`OCIStmtPrepare2`、`OCIStmtExecute` |
  | `oci7` | `ociw32.dll`、`oci.dll` | `oparse`、`osql3`、`oexec`、`oexn`、`oexfet` |
  | `dblib` | `ntwdblib.dll` | `dbcmd`、`dbsqlexec`、`dbsqlsend` |

  ODBC のドライバーが内側で OCI などを呼んだ分は、重ねて出しません（`--keep-nested` で出せます）。
  準備（Prepare）と実行（Execute）は、同じハンドルどうしで対応付けて、実行の行にも SQL を出します。
- 動いているプロセスに付く（アタッチ）ことも、起動して最初から捕まえることもできます。
- x64 版は 64 ビットと 32 ビット（WOW64）のどちらのプロセスも扱えます。
- 出力: コンソール（SQL を色分けします）、HTML（1 ファイルで完結し、絞り込みができます）、Excel（xlsx）、
  NDJSON（生の記録。後から HTML や Excel に書き出せます）。
- GUI（`sqltrace-gui.exe`）とコマンドライン（`sqltrace.exe`）の両方があります。

## 入手

[Releases](../../releases) から zip を取ってきて、好きな場所に展開します。インストールはいりません。

- `x64`: 64 ビットの Windows ではこちらを使います（32 ビットのアプリも追えます）。
- `x86`: 32 ビットの Windows で使います。

## 使い方（GUI）

`sqltrace-gui.exe` を開き、次のどちらかで対象を選んで「▶ 開始」を押します。

- **アタッチ**: プロセスの一覧から選ぶか、「◎ 照準」を押したまま対象のウィンドウまでドラッグして離します。
- **起動**: 実行ファイル、引数、作業フォルダーを指定します。

捕まえた SQL は表に出ます。「実行だけ」で実行の行だけに絞り込めます。保存先を指定すると、HTML、Excel、
NDJSON に書き出せます。「■ 離れる」でブレークポイントを外して離れます。

## 使い方（コマンドライン）

```text
sqltrace attach <PID> [オプション]           動いているプロセスにアタッチする
sqltrace run [オプション] -- <EXE> [引数...]  起動して最初からトレースする
sqltrace export <記録.ndjson> [出力]          保存した記録を HTML や Excel に書き出す
sqltrace repair <PID>                        強制終了で残ったブレークポイントを元に戻す
sqltrace layers                              捕まえられる層の一覧
```

例:

```powershell
# PID 1234 に付き、SQL を画面に出しながら HTML と Excel にも残す
sqltrace attach 1234 --html trace.html --xlsx trace.xlsx

# ODBC だけを捕まえて起動する。生の記録を残しておき、後で書き出す
sqltrace run --layers odbc --ndjson trace.ndjson -- "C:\App\app.exe" /arg
sqltrace export trace.ndjson --html trace.html
```

止めるときは Ctrl+C を押します。SQL は標準出力に、状態と警告は標準エラーに出るので、
`sqltrace attach 1234 > sql.txt` で SQL だけをファイルに取れます。

オプションの一覧は `sqltrace help` で出ます。

## 注意

- 対象のプロセスをデバッグする権限が要ります。管理者として動いているプロセスに付くには、sqltrace も管理者として
  動かしてください。
- 1 つのプロセスに付けられるデバッガは 1 つです。Visual Studio などのデバッガがもう付いていれば使えません。
  同じプロセスに 2 つ目の sqltrace を付けようとしても断ります。
- トレースの間は、DB の API を呼ぶたびに対象が一瞬止まるので、少し遅くなります。
- **sqltrace を強制終了すると、対象にブレークポイントが残ります。** 残ったまま API を呼ぶと、対象は落ちます。
  強制終了してしまったときは、すぐに `sqltrace repair <PID>` を実行してください。ふつうに止めれば
  （Ctrl+C、「■ 離れる」、窓を閉じる）、ブレークポイントはきちんと外れます。
- 0.1.0 で読み取るのは SQL の文だけです。戻り値、かかった時間、バインド変数の値は、まだ取れません。
- Pro*C、JDBC と、ネイティブの API を通らない .NET のプロバイダー（ODP.NET Managed、SqlClient など）には、
  まだ対応していません。

## ビルド

Rust（バージョンは `rust-toolchain.toml` で決めています）と、MSVC のビルドツールが要ります。

```powershell
cargo build --release --target x86_64-pc-windows-msvc
```

## ライセンス

MIT（[LICENSE](LICENSE)）
