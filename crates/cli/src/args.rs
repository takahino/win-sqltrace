//! コマンドラインの解釈（OS に依存しない純粋な処理）。

use std::path::PathBuf;

pub const USAGE: &str = "\
使い方:
  sqltrace attach <PID> [オプション]           動いているプロセスにアタッチする
  sqltrace run [オプション] -- <EXE> [引数...]  起動して最初からトレースする
  sqltrace export <記録.ndjson> [出力]          保存した記録を HTML や Excel に書き出す
  sqltrace repair <PID>                        強制終了で残ったブレークポイントを元に戻す
  sqltrace layers                              捕まえられる層の一覧

オプション:
  --layers <odbc,oci,...>  捕まえる層（既定はすべて）
  --keep-nested            DB の API の内側からの呼び出しも出す（重複の除去を切る）
  --cwd <DIR>              run の作業フォルダー（既定は EXE のあるフォルダー）
  --env <名前=値>          run の環境変数を足す（何度でも指定できる）
  --same-console           run の対象を新しいコンソールで開かず、この画面を共有する

出力（attach、run、export）:
  --html <FILE>            HTML の報告を書く（トレースの終わりに書く）
  --xlsx <FILE>            Excel の報告を書く（トレースの終わりに書く）
  --ndjson <FILE>          生の記録を 1 件ずつ書く（後で sqltrace export で書き出せる）
  --quiet                  SQL をコンソールに出さない
  --no-color               コンソールの SQL を色分けしない（リダイレクトしたときは、もともと付けない）

止めるには Ctrl+C を押します（ブレークポイントを外して離れ、対象は動き続けます）。";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Outputs {
    pub html: Option<PathBuf>,
    pub xlsx: Option<PathBuf>,
    pub ndjson: Option<PathBuf>,
    pub quiet: bool,
    pub no_color: bool,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Common {
    pub layers: Option<String>,
    pub keep_nested: bool,
    pub outputs: Outputs,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Run {
    pub exe: PathBuf,
    pub arguments: Vec<String>,
    pub working_dir: Option<PathBuf>,
    pub environment: Vec<(String, String)>,
    pub same_console: bool,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Command {
    Attach { pid: u32, common: Common },
    Run { run: Run, common: Common },
    Export { input: PathBuf, outputs: Outputs },
    Repair { pid: u32 },
    Layers,
    Help,
}

/// 誤りの説明（使い方と一緒に出す）。
#[derive(Debug, PartialEq, Eq)]
pub struct UsageError(pub String);

fn pid(value: Option<String>) -> Result<u32, UsageError> {
    let value = value.ok_or_else(|| UsageError("PID を指定してください".into()))?;
    value
        .parse()
        .map_err(|_| UsageError(format!("PID は数で指定してください（'{value}'）")))
}

pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Command, UsageError> {
    let mut args = args.into_iter();
    let Some(command) = args.next() else {
        return Ok(Command::Help);
    };
    match command.as_str() {
        "help" | "--help" | "-h" | "/?" => Ok(Command::Help),
        "layers" => Ok(Command::Layers),
        "repair" => Ok(Command::Repair { pid: pid(args.next())? }),
        "export" => {
            let input = args
                .next()
                .ok_or_else(|| UsageError("書き出す記録（.ndjson）を指定してください".into()))?;
            let mut outputs = Outputs::default();
            while let Some(arg) = args.next() {
                if !output_option(&arg, &mut args, &mut outputs)? {
                    return Err(UsageError(format!("export では '{arg}' は使えません")));
                }
            }
            if outputs.ndjson.is_some() {
                return Err(UsageError(
                    "export の出力は --html か --xlsx です（記録はもう NDJSON です）".into(),
                ));
            }
            Ok(Command::Export {
                input: input.into(),
                outputs,
            })
        }
        "attach" => {
            let pid = pid(args.next())?;
            let mut common = Common::default();
            while let Some(arg) = args.next() {
                if !common_option(&arg, &mut args, &mut common)? {
                    return Err(UsageError(format!("attach では '{arg}' は使えません")));
                }
            }
            Ok(Command::Attach { pid, common })
        }
        "run" => {
            let mut common = Common::default();
            let mut working_dir = None;
            let mut environment = Vec::new();
            let mut same_console = false;
            loop {
                let arg = args
                    .next()
                    .ok_or_else(|| UsageError("'--' の後に起動する EXE を指定してください".into()))?;
                match arg.as_str() {
                    "--" => break,
                    "--cwd" => working_dir = Some(PathBuf::from(value(&arg, &mut args)?)),
                    "--same-console" => same_console = true,
                    "--env" => {
                        let pair = value(&arg, &mut args)?;
                        let (name, val) = pair
                            .split_once('=')
                            .filter(|(n, _)| !n.is_empty())
                            .ok_or_else(|| UsageError(format!("--env は 名前=値 で指定してください（'{pair}'）")))?;
                        environment.push((name.to_owned(), val.to_owned()));
                    }
                    _ if common_option(&arg, &mut args, &mut common)? => {}
                    _ => {
                        return Err(UsageError(format!(
                            "'{arg}' は知らないオプションです。起動する EXE は '--' の後に書いてください"
                        )))
                    }
                }
            }
            let exe = args
                .next()
                .ok_or_else(|| UsageError("'--' の後に起動する EXE を指定してください".into()))?;
            Ok(Command::Run {
                run: Run {
                    exe: exe.into(),
                    arguments: args.collect(),
                    working_dir,
                    environment,
                    same_console,
                },
                common,
            })
        }
        other => Err(UsageError(format!("'{other}' というコマンドはありません"))),
    }
}

fn value(name: &str, args: &mut impl Iterator<Item = String>) -> Result<String, UsageError> {
    args.next()
        .ok_or_else(|| UsageError(format!("{name} の値を指定してください")))
}

/// attach と run で共通のオプションなら読んで `true`。
fn common_option(arg: &str, args: &mut impl Iterator<Item = String>, common: &mut Common) -> Result<bool, UsageError> {
    match arg {
        "--layers" => common.layers = Some(value(arg, args)?),
        "--keep-nested" => common.keep_nested = true,
        _ => return output_option(arg, args, &mut common.outputs),
    }
    Ok(true)
}

/// 出力のオプションなら読んで `true`。
fn output_option(
    arg: &str,
    args: &mut impl Iterator<Item = String>,
    outputs: &mut Outputs,
) -> Result<bool, UsageError> {
    match arg {
        "--html" => outputs.html = Some(value(arg, args)?.into()),
        "--xlsx" => outputs.xlsx = Some(value(arg, args)?.into()),
        "--ndjson" => outputs.ndjson = Some(value(arg, args)?.into()),
        "--quiet" => outputs.quiet = true,
        "--no-color" => outputs.no_color = true,
        _ => return Ok(false),
    }
    Ok(true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse_str(line: &str) -> Result<Command, UsageError> {
        parse(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn attach_with_layers() {
        assert_eq!(
            parse_str("attach 1234 --layers odbc,oci").unwrap(),
            Command::Attach {
                pid: 1234,
                common: Common {
                    layers: Some("odbc,oci".into()),
                    ..Common::default()
                }
            }
        );
        assert!(parse_str("attach abc").is_err());
        assert!(parse_str("attach").is_err());
    }

    #[test]
    fn run_passes_everything_after_double_dash_to_the_program() {
        let Command::Run { run, common } =
            parse_str("run --cwd C:\\w --env A=1 --keep-nested -- app.exe --layers x -- y").unwrap()
        else {
            panic!("run ではない");
        };
        assert_eq!(run.exe, PathBuf::from("app.exe"));
        assert_eq!(run.arguments, ["--layers", "x", "--", "y"]);
        assert_eq!(run.working_dir, Some(PathBuf::from("C:\\w")));
        assert_eq!(run.environment, [("A".to_owned(), "1".to_owned())]);
        assert!(common.keep_nested && common.layers.is_none());
    }

    #[test]
    fn run_requires_double_dash_and_exe() {
        assert!(parse_str("run app.exe").is_err());
        assert!(parse_str("run --").is_err());
        assert!(parse_str("run --env =x -- a.exe").is_err());
    }

    #[test]
    fn outputs_are_accepted_by_attach_run_and_export() {
        let Command::Attach { common, .. } =
            parse_str("attach 1 --html r.html --xlsx r.xlsx --ndjson r.ndjson --quiet --no-color").unwrap()
        else {
            panic!("attach ではない");
        };
        assert_eq!(
            common.outputs,
            Outputs {
                html: Some("r.html".into()),
                xlsx: Some("r.xlsx".into()),
                ndjson: Some("r.ndjson".into()),
                quiet: true,
                no_color: true,
            }
        );
        let Command::Run { common, run } = parse_str("run --html r.html -- app.exe --html x").unwrap() else {
            panic!("run ではない");
        };
        // '--' の後ろは対象の引数。
        assert_eq!(common.outputs.html, Some("r.html".into()));
        assert_eq!(run.arguments, ["--html", "x"]);
        assert_eq!(
            parse_str("export log.ndjson --xlsx r.xlsx").unwrap(),
            Command::Export {
                input: "log.ndjson".into(),
                outputs: Outputs {
                    xlsx: Some("r.xlsx".into()),
                    ..Outputs::default()
                }
            }
        );
        assert!(parse_str("export log.ndjson --ndjson x").is_err());
        assert!(parse_str("export").is_err());
    }

    #[test]
    fn no_arguments_is_help() {
        assert_eq!(parse_str("").unwrap(), Command::Help);
        assert!(parse_str("frobnicate").is_err());
    }
}
