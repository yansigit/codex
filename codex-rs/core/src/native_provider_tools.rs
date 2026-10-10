//! Cursor operations translated to the executable Codex shell tool. No host I/O
//! happens here: execution, permission checks and hooks remain in ToolCallRuntime.
use base64::Engine;
use base64::engine::general_purpose::STANDARD;
use codex_provider_api::{ProviderError, Result, ToolDefinition};
use serde_json::{Value, json};

const MARKER: &str = "CODEX_CURSOR_RESULT_V1:";

pub(super) fn definitions() -> Vec<ToolDefinition> {
    [
        ("cursor_shell", "Run a shell command under Codex permissions", json!({"command":{"type":"string"},"working_directory":{"type":"string"},"timeout_ms":{"type":"integer"}}), vec!["command"]),
        ("cursor_read_file", "Read a file under Codex permissions", json!({"path":{"type":"string"},"offset":{"type":"integer"},"limit":{"type":"integer"}}), vec!["path"]),
        ("cursor_write_file", "Write a file under Codex permissions", json!({"path":{"type":"string"},"content":{"type":"string"}}), vec!["path","content"]),
        ("cursor_delete_file", "Delete a file under Codex permissions", json!({"path":{"type":"string"}}), vec!["path"]),
        ("cursor_ls", "List a directory under Codex permissions", json!({"path":{"type":"string"},"ignore":{"type":"array","items":{"type":"string"}}}), vec!["path"]),
        ("cursor_grep", "Search files under Codex permissions", json!({"pattern":{"type":"string"},"path":{"type":"string"},"glob":{"type":"string"},"context":{"type":"integer"},"context_before":{"type":"integer"},"context_after":{"type":"integer"},"case_insensitive":{"type":"boolean"},"type":{"type":"string"},"head_limit":{"type":"integer"},"multiline":{"type":"boolean"},"sort":{"type":"string"},"sort_ascending":{"type":"boolean"},"offset":{"type":"integer"},"output_mode":{"type":"string"}}), vec!["pattern"]),
    ].into_iter().map(|(name,description,properties,required)| ToolDefinition { name:name.into(),description:description.into(),parameters:json!({"type":"object","properties":properties,"required":required}) }).collect()
}

fn text<'a>(args: &'a Value, name: &str) -> Result<&'a str> {
    args[name]
        .as_str()
        .filter(|value| !value.contains('\0'))
        .ok_or_else(|| ProviderError::Host(format!("missing or invalid {name}")))
}
fn quote(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\"'\"'"))
}

fn integer(args: &Value, name: &str, default: u64, min: u64, max: u64) -> Result<u64> {
    let Some(value) = args.get(name) else {
        return Ok(default);
    };
    let value = value
        .as_u64()
        .ok_or_else(|| ProviderError::Host(format!("{name} must be a non-negative integer")))?;
    if !(min..=max).contains(&value) {
        return Err(ProviderError::Host(format!(
            "{name} must be between {min} and {max}"
        )));
    }
    Ok(value)
}

fn optional_text<'a>(args: &'a Value, name: &str) -> Result<Option<&'a str>> {
    match args.get(name) {
        None | Some(Value::Null) => Ok(None),
        Some(value) => value
            .as_str()
            .filter(|value| !value.contains('\0'))
            .map(Some)
            .ok_or_else(|| ProviderError::Host(format!("{name} must be a string without NUL"))),
    }
}

fn bool_option(args: &Value, name: &str) -> Result<bool> {
    match args.get(name) {
        None => Ok(false),
        Some(value) => value
            .as_bool()
            .ok_or_else(|| ProviderError::Host(format!("{name} must be a boolean"))),
    }
}

fn line_count_script() -> &'static str {
    r#"lines=$(wc -l <"$p")
bytes=$(wc -c <"$p")
if [ "$bytes" -gt 0 ] && [ "$(tail -c 1 -- "$p" | od -An -t x1 | tr -d ' \n')" != 0a ]; then lines=$((lines + 1)); fi"#
}

pub(super) fn command(name: &str, args: &Value) -> Result<Value> {
    let script = match name {
        "cursor_shell" => {
            let command = quote(text(args, "command")?);
            let seconds = integer(args, "timeout_ms", 120_000, 1, 3_600_000)?.div_ceil(1000);
            // A private temp directory captures streams separately and is cleaned by the shell.
            format!(
                "d=$(mktemp -d) || exit 1\ntrap 'rm -- \"$d/out\" \"$d/err\"; rmdir -- \"$d\"' EXIT\ntimeout --signal=TERM --kill-after=5s {seconds}s /bin/bash -lc {command} >\"$d/out\" 2>\"$d/err\"\ns=$?\nprintf '{MARKER}%s\\n' \"$s\"\nbase64 -w0 <\"$d/out\"\nprintf '\\n'\nbase64 -w0 <\"$d/err\"\nprintf '\\n'"
            )
        }
        "cursor_read_file" => {
            let path = quote(text(args, "path")?);
            let start = integer(args, "offset", 1, 1, i32::MAX as u64)?;
            let limit = integer(args, "limit", 2000, 1, 10000)?;
            let end = start.saturating_add(limit - 1);
            format!(
                "p={path}\ntest -f \"$p\" || exit 1\n{}\nprintf '{MARKER}%s %s\\n' \"$lines\" \"$bytes\"\nsed -n '{start},{end}p' -- \"$p\" | head -c 65536 | base64 -w0\nprintf '\\n'",
                line_count_script()
            )
        }
        "cursor_write_file" => {
            let path = quote(text(args, "path")?);
            let content = text(args, "content")?;
            if content.len() > 1024 * 1024 {
                return Err(ProviderError::Host("native write exceeds 1 MiB".into()));
            }
            let encoded = STANDARD.encode(content);
            format!(
                "p={path}\nprintf '%s' '{encoded}' | base64 -d >\"$p\" || exit 1\n{}\nprintf '{MARKER}%s %s\\n' \"$lines\" \"$bytes\"",
                line_count_script()
            )
        }
        "cursor_delete_file" => {
            let path = quote(text(args, "path")?);
            format!("test -f {path} && rm -- {path} && printf '{MARKER}ok\\n'")
        }
        "cursor_ls" => {
            let path = quote(text(args, "path")?);
            let ignores = match args.get("ignore") {
                None => Vec::new(),
                Some(Value::Array(values)) => values
                    .iter()
                    .map(|value| {
                        value
                            .as_str()
                            .filter(|s| !s.contains('\0'))
                            .map(quote)
                            .ok_or_else(|| {
                                ProviderError::Host(
                                    "ignore entries must be strings without NUL".into(),
                                )
                            })
                    })
                    .collect::<Result<Vec<_>>>()?,
                _ => {
                    return Err(ProviderError::Host(
                        "ignore must be an array of strings".into(),
                    ));
                }
            };
            let mut predicates = String::new();
            for ignore in ignores {
                predicates.push_str(&format!(" ! -name {ignore}"));
            }
            format!(
                "p={path}\nb=$(cd -- \"$p\" && pwd -P) || exit 1\nprintf '{MARKER}ls\\n%s\\n' \"$b\"\nfind \"$p\" -mindepth 1 -maxdepth 1{predicates} -printf '%y\\t%f\\0' | base64 -w0\nprintf '\\n'"
            )
        }
        "cursor_grep" => {
            let pattern = quote(text(args, "pattern")?);
            let path = quote(optional_text(args, "path")?.unwrap_or("."));
            let glob = optional_text(args, "glob")?
                .map(|value| format!(" --glob {}", quote(value)))
                .unwrap_or_default();
            let kind = optional_text(args, "type")?
                .map(|value| format!(" --type {}", quote(value)))
                .unwrap_or_default();
            let case = if bool_option(args, "case_insensitive")? {
                " --ignore-case"
            } else {
                ""
            };
            let multiline = if bool_option(args, "multiline")? {
                " --multiline"
            } else {
                ""
            };
            let context = args
                .get("context")
                .map(|_| integer(args, "context", 0, 0, i32::MAX as u64))
                .transpose()?;
            let before = args
                .get("context_before")
                .map(|_| integer(args, "context_before", 0, 0, i32::MAX as u64))
                .transpose()?
                .or(context);
            let after = args
                .get("context_after")
                .map(|_| integer(args, "context_after", 0, 0, i32::MAX as u64))
                .transpose()?
                .or(context);
            let before = before
                .map(|value| format!(" --before-context {value}"))
                .unwrap_or_default();
            let after = after
                .map(|value| format!(" --after-context {value}"))
                .unwrap_or_default();
            let sort = match optional_text(args, "sort")? {
                None | Some("path") => " --sort path",
                Some("none") => "",
                Some(other) => {
                    return Err(ProviderError::Unsupported(format!(
                        "Cursor grep sort mode {other:?} is unsupported"
                    )));
                }
            };
            let sort_ascending = match args.get("sort_ascending") {
                None => true,
                Some(value) => value.as_bool().ok_or_else(|| {
                    ProviderError::Host("sort_ascending must be a boolean".into())
                })?,
            };
            if sort.is_empty() && !sort_ascending {
                return Err(ProviderError::Unsupported(
                    "sort_ascending requires path sorting".into(),
                ));
            }
            let _ = integer(args, "offset", 0, 0, i32::MAX as u64)?;
            let _ = integer(args, "head_limit", i32::MAX as u64, 0, i32::MAX as u64)?;
            let output_mode = optional_text(args, "output_mode")?.unwrap_or("content");
            if !matches!(output_mode, "content" | "files_with_matches" | "count") {
                return Err(ProviderError::Unsupported(format!(
                    "Cursor grep output mode {output_mode:?} is unsupported"
                )));
            }
            format!(
                "printf '{MARKER}grep\\n'\nrg --json{case}{multiline}{before}{after}{glob}{kind}{sort} -- {pattern} {path}\ns=$?\ntest \"$s\" -le 1"
            )
        }
        _ => {
            return Err(ProviderError::Unsupported(
                "unknown Cursor operation".into(),
            ));
        }
    };
    let mut result = json!({"cmd":script,"shell":"/bin/bash","login":false,"yield_time_ms":1000,"max_output_tokens":30000});
    if let Some(directory) = optional_text(args, "working_directory")? {
        result["workdir"] = json!(directory);
    }
    Ok(result)
}

pub(super) fn result(name: &str, args: &Value, structured: &Value) -> Result<Value> {
    if structured["exit_code"]
        .as_i64()
        .is_some_and(|code| code != 0)
    {
        return Err(ProviderError::Host(
            "native operation failed inside Codex sandbox".into(),
        ));
    }
    let output = structured["output"]
        .as_str()
        .ok_or_else(|| ProviderError::Host("missing structured shell output".into()))?;
    let output = output.strip_prefix(MARKER).ok_or_else(|| {
        ProviderError::Host("native operation output was truncated or changed".into())
    })?;
    let mut lines = output.lines();
    let header = lines.next().unwrap_or_default();
    let decode = |text: &str| -> Result<String> {
        let data = STANDARD
            .decode(text)
            .map_err(|_| ProviderError::Host("invalid encoded native output".into()))?;
        String::from_utf8(data)
            .map_err(|_| ProviderError::Host("binary native output is unsupported".into()))
    };
    let decode_read = |text: &str| -> Result<String> {
        let bytes = STANDARD
            .decode(text)
            .map_err(|_| ProviderError::Host("invalid encoded native output".into()))?;
        match String::from_utf8(bytes) {
            Ok(text) => Ok(text),
            Err(error)
                if error.utf8_error().error_len().is_none() && error.as_bytes().len() == 65_536 =>
            {
                let valid_up_to = error.utf8_error().valid_up_to();
                String::from_utf8(error.into_bytes()[..valid_up_to].to_vec())
                    .map_err(|_| ProviderError::Host("invalid UTF-8 native file content".into()))
            }
            Err(_) => Err(ProviderError::Host(
                "invalid UTF-8 native file content".into(),
            )),
        }
    };
    match name {
        "cursor_shell" => Ok(
            json!({"exit_code":header.parse::<i32>().map_err(|_|ProviderError::Host("missing native shell status".into()))?,"stdout":decode(lines.next().unwrap_or_default())?,"stderr":decode(lines.next().unwrap_or_default())?}),
        ),
        "cursor_read_file" | "cursor_write_file" => {
            let mut sizes = header.split_whitespace();
            let count = sizes
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| ProviderError::Host("missing native file line count".into()))?;
            let size = sizes
                .next()
                .and_then(|v| v.parse::<u64>().ok())
                .ok_or_else(|| ProviderError::Host("missing native file size".into()))?;
            if name == "cursor_write_file" {
                return Ok(json!({"lines_created":count,"file_size":size}));
            }
            let content = decode_read(lines.next().unwrap_or_default())?;
            Ok(
                json!({"truncated":(content.len() as u64) < size,"content":content,"total_lines":count,"file_size":size,"range_applied":true}),
            )
        }
        "cursor_delete_file" => Ok(json!({})),
        "cursor_ls" => {
            if header != "ls" {
                return Err(ProviderError::Host(
                    "missing native directory header".into(),
                ));
            }
            let base = lines.next().unwrap_or_default();
            if !std::path::Path::new(base).is_absolute() {
                return Err(ProviderError::Host(
                    "native directory path is not absolute".into(),
                ));
            }
            let records = decode(lines.next().unwrap_or_default())?;
            let mut files = vec![];
            let mut directories = vec![];
            for record in records.split('\0').filter(|record| !record.is_empty()) {
                let Some((kind, name)) = record.split_once('\t') else {
                    return Err(ProviderError::Host("invalid native directory entry".into()));
                };
                if kind == "d" {
                    directories.push(
                        std::path::Path::new(base)
                            .join(name)
                            .to_string_lossy()
                            .to_string(),
                    );
                } else {
                    files.push(name.to_owned());
                }
            }
            Ok(json!({"files":files,"directories":directories}))
        }
        "cursor_grep" => {
            let mut matches = Vec::<(String, Vec<Value>)>::new();
            for line in lines {
                let value: Value = serde_json::from_str(line)
                    .map_err(|_| ProviderError::Host("invalid native search output".into()))?;
                if value["type"] == "match" || value["type"] == "context" {
                    let data = &value["data"];
                    let path = data["path"]["text"]
                        .as_str()
                        .ok_or_else(|| ProviderError::Host("non-UTF8 search path".into()))?;
                    let index = matches
                        .iter()
                        .position(|(file, _)| file == path)
                        .unwrap_or_else(|| {
                            matches.push((path.to_owned(), Vec::new()));
                            matches.len() - 1
                        });
                    matches[index].1.push(json!({"line_number":data["line_number"],"content":data["lines"]["text"],"is_context_line":value["type"]=="context"}));
                }
            }
            let sort = args["sort"].as_str().unwrap_or("path");
            if sort == "path" {
                matches.sort_by(|left, right| left.0.cmp(&right.0));
            }
            let ascending = match args.get("sort_ascending") {
                None => true,
                Some(value) => value.as_bool().ok_or_else(|| {
                    ProviderError::Host("sort_ascending must be a boolean".into())
                })?,
            };
            if sort == "path" && !ascending {
                matches.reverse();
            } else if sort == "none" && !ascending {
                return Err(ProviderError::Unsupported(
                    "sort_ascending requires path sorting".into(),
                ));
            }
            let offset = integer(args, "offset", 0, 0, i32::MAX as u64)? as usize;
            let limit = args
                .get("head_limit")
                .map(|_| integer(args, "head_limit", 0, 0, i32::MAX as u64))
                .transpose()?
                .map(|n| n as usize);
            let mode = args["output_mode"].as_str().unwrap_or("content");
            let mut remaining = limit.unwrap_or(usize::MAX);
            let mut selected = Vec::new();
            let mut index = 0usize;
            for (file, rows) in matches {
                let original_len = rows.len();
                let rows = rows
                    .into_iter()
                    .skip(offset.saturating_sub(index))
                    .take(remaining)
                    .collect::<Vec<_>>();
                index = index.saturating_add(original_len);
                remaining = remaining.saturating_sub(rows.len());
                if !rows.is_empty() {
                    selected.push((file, rows));
                }
                if remaining == 0 {
                    break;
                }
            }
            match mode {
                "files_with_matches" => {
                    Ok(json!({"files":selected.iter().map(|(file,_)|file).collect::<Vec<_>>()}))
                }
                "count" => Ok(
                    json!({"counts":selected.iter().map(|(file,rows)|json!({"file":file,"count":rows.iter().filter(|row| row["is_context_line"] == false).count()})).collect::<Vec<_>>()}),
                ),
                "content" => Ok(
                    json!({"matches":selected.into_iter().map(|(file,lines)|json!({"file":file,"lines":lines})).collect::<Vec<_>>()}),
                ),
                _ => Err(ProviderError::Unsupported(
                    "unsupported Cursor grep output mode".into(),
                )),
            }
        }
        _ => Err(ProviderError::Unsupported(
            "unknown native operation".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{MARKER, command, result};
    use base64::Engine;
    use base64::engine::general_purpose::STANDARD;
    use serde_json::{Value, json};
    use std::process::Command;

    fn run_generated(name: &str, args: &Value, cwd: &std::path::Path) -> Value {
        let generated = command(name, args).expect("operation command generation");
        let output = Command::new("/bin/bash")
            .arg("-c")
            .arg(generated["cmd"].as_str().expect("generated shell command"))
            .current_dir(cwd)
            .output()
            .expect("run known operation against temp fixture");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        json!({"exit_code":output.status.code(),"output":String::from_utf8(output.stdout).expect("command output is UTF-8")})
    }

    #[test]
    fn command_shell_quotes_arguments_and_validates_options() {
        let generated = command(
            "cursor_grep",
            &json!({
                "pattern":"a'b", "path":"src files", "glob":"*.rs", "case_insensitive":true,
                "context":2, "context_before":3, "context_after":4, "type":"rust",
                "head_limit":8, "multiline":true, "sort":"path", "offset":1
            }),
        )
        .expect("grep command");
        let script = generated["cmd"].as_str().unwrap();
        assert!(script.contains("'a'\"'\"'b'"));
        assert!(script.contains("--glob '*.rs'"));
        assert!(script.contains("--ignore-case"));
        assert!(script.contains("--multiline"));
        assert!(script.contains("--before-context 3"));
        assert!(script.contains("--after-context 4"));
        assert!(script.contains("--type 'rust'"));
        assert!(script.contains("--sort path"));
        assert!(command("cursor_grep", &json!({"pattern":"x", "sort":"modified"})).is_err());
        assert!(command("cursor_grep", &json!({"pattern":"x", "offset":-1})).is_err());
        assert!(command("cursor_read_file", &json!({"path":"bad\0path"})).is_err());
    }

    #[test]
    fn read_and_write_commands_count_unterminated_final_lines_and_bound_utf8() {
        let read = command(
            "cursor_read_file",
            &json!({"path":"notes.txt", "offset":1, "limit":3}),
        )
        .unwrap();
        let script = read["cmd"].as_str().unwrap();
        assert!(script.contains("lines=$((lines + 1))"));
        assert!(script.contains("head -c 65536 | base64 -w0"));
        let write = command(
            "cursor_write_file",
            &json!({"path":"notes.txt", "content":"end"}),
        )
        .unwrap();
        assert!(write["cmd"].as_str().unwrap().contains("tail -c 1"));
        assert!(command("cursor_read_file", &json!({"path":"x", "limit":0})).is_err());
    }

    #[test]
    fn ls_ignores_requested_names_and_returns_absolute_directory_paths() {
        let generated = command(
            "cursor_ls",
            &json!({"path":".", "ignore":[".git", "target"]}),
        )
        .unwrap();
        let script = generated["cmd"].as_str().unwrap();
        assert!(script.contains("pwd -P"));
        assert!(script.contains("! -name '.git' ! -name 'target'"));

        let entries = STANDARD.encode("d\tchild\0f\tfile.txt\0");
        let structured = json!({"output":format!("{MARKER}ls\n/work/tree\n{entries}\n")});
        let parsed = result("cursor_ls", &json!({"path":"."}), &structured).unwrap();
        assert_eq!(
            parsed,
            json!({"files":["file.txt"],"directories":["/work/tree/child"]})
        );
        assert!(command("cursor_ls", &json!({"path":".", "ignore":".git"})).is_err());
    }

    #[test]
    fn grep_parses_context_paginates_and_rejects_unknown_modes() {
        let records = [
            json!({"type":"match","data":{"path":{"text":"b.rs"},"line_number":2,"lines":{"text":"hit\n"}}}),
            json!({"type":"context","data":{"path":{"text":"b.rs"},"line_number":3,"lines":{"text":"after\n"}}}),
            json!({"type":"match","data":{"path":{"text":"a.rs"},"line_number":1,"lines":{"text":"other\n"}}}),
        ].iter().map(Value::to_string).collect::<Vec<_>>().join("\n");
        let structured = json!({"output":format!("{MARKER}grep\n{records}\n")});
        let parsed = result(
            "cursor_grep",
            &json!({"offset":1,"head_limit":1,"sort":"none"}),
            &structured,
        )
        .unwrap();
        assert_eq!(
            parsed,
            json!({"matches":[{"file":"b.rs","lines":[{"line_number":3,"content":"after\n","is_context_line":true}]}]})
        );
        let counted = result(
            "cursor_grep",
            &json!({"output_mode":"count", "sort":"none"}),
            &structured,
        )
        .unwrap();
        assert_eq!(counted["counts"][0]["count"], 1);

        let descending = result(
            "cursor_grep",
            &json!({"sort":"path", "sort_ascending":false}),
            &structured,
        )
        .unwrap();
        assert_eq!(descending["matches"][0]["file"], "b.rs");
        assert!(command("cursor_grep", &json!({"pattern":"x", "output_mode":"bad"})).is_err());
    }

    #[test]
    fn malformed_result_shapes_fail_closed() {
        assert!(
            result(
                "cursor_read_file",
                &json!({}),
                &json!({"output":"missing marker"})
            )
            .is_err()
        );
        let invalid = format!("{MARKER}grep\nnot-json\n");
        assert!(result("cursor_grep", &json!({}), &json!({"output":invalid})).is_err());
    }

    #[test]
    fn read_clips_only_an_incomplete_utf8_sequence_at_the_output_cap() {
        let mut capped = vec![b'a'; 65_535];
        capped.push(0xE2);
        let encoded = STANDARD.encode(capped);
        let structured = json!({"output":format!("{MARKER}1 65537\n{encoded}\n")});
        let parsed = result("cursor_read_file", &json!({}), &structured).unwrap();
        assert_eq!(parsed["content"].as_str().unwrap().len(), 65_535);

        let malformed = STANDARD.encode([0xFF]);
        let structured = json!({"output":format!("{MARKER}1 1\n{malformed}\n")});
        assert!(result("cursor_read_file", &json!({}), &structured).is_err());

        let mut malformed_at_cap = vec![b'a'; 65_535];
        malformed_at_cap.push(0xFF);
        let structured =
            json!({"output":format!("{MARKER}1 65536\n{}\n", STANDARD.encode(malformed_at_cap))});
        assert!(result("cursor_read_file", &json!({}), &structured).is_err());
    }

    #[test]
    fn generated_read_and_write_commands_work_on_quoted_temp_paths() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("file'quoted.txt");
        std::fs::write(&path, b"one\nlast").expect("fixture");
        let args = json!({"path":path.to_string_lossy(), "offset":1, "limit":10});
        let structured = run_generated("cursor_read_file", &args, temp.path());
        let parsed = result("cursor_read_file", &args, &structured).expect("read result");
        assert_eq!(parsed["content"], "one\nlast");
        assert_eq!(parsed["total_lines"], 2);
        assert_eq!(parsed["file_size"], 8);

        let content = "written\nlast without newline";
        let write_args = json!({"path":path.to_string_lossy(), "content":content});
        let structured = run_generated("cursor_write_file", &write_args, temp.path());
        let parsed = result("cursor_write_file", &write_args, &structured).expect("write result");
        assert_eq!(
            std::fs::read_to_string(path).expect("written fixture"),
            content
        );
        assert_eq!(parsed["lines_created"], 2);
    }

    #[test]
    fn generated_read_handles_utf8_cap_and_rejects_binary_fixture() {
        let temp = tempfile::tempdir().expect("tempdir");
        let path = temp.path().join("unicode.txt");
        let mut bytes = vec![b'a'; 65_535];
        bytes.extend_from_slice("é".as_bytes());
        std::fs::write(&path, &bytes).expect("unicode fixture");
        let args = json!({"path":path.to_string_lossy()});
        let structured = run_generated("cursor_read_file", &args, temp.path());
        let parsed = result("cursor_read_file", &args, &structured).expect("bounded utf8 read");
        assert_eq!(parsed["content"].as_str().unwrap().len(), 65_535);
        assert_eq!(parsed["truncated"], true);

        std::fs::write(&path, [b'a', 0xff, b'b']).expect("binary fixture");
        let structured = run_generated("cursor_read_file", &args, temp.path());
        assert!(result("cursor_read_file", &args, &structured).is_err());
    }

    #[test]
    fn generated_grep_and_ls_apply_options_on_temp_fixtures() {
        let temp = tempfile::tempdir().expect("tempdir");
        let source = temp.path().join("one.rs");
        std::fs::write(&source, "before\nNeedle\nafter\n").expect("source fixture");
        std::fs::write(temp.path().join("ignored.txt"), "needle\n").expect("other fixture");
        let grep_args = json!({
            "pattern":"needle", "path":temp.path().to_string_lossy(), "glob":"*.rs",
            "context_before":1, "context_after":1, "case_insensitive":true, "type":"rust",
            "head_limit":10, "multiline":false, "sort":"path", "output_mode":"content"
        });
        let structured = run_generated("cursor_grep", &grep_args, temp.path());
        let parsed = result("cursor_grep", &grep_args, &structured).expect("grep result");
        assert_eq!(
            parsed["matches"][0]["file"],
            source.to_string_lossy().as_ref()
        );
        let rows = parsed["matches"][0]["lines"].as_array().unwrap();
        assert_eq!(rows.len(), 3);
        assert_eq!(rows[1]["is_context_line"], false);

        let count_args = json!({"pattern":"needle", "path":temp.path().to_string_lossy(), "glob":"*.rs", "context":1, "case_insensitive":true, "type":"rust", "output_mode":"count"});
        let structured = run_generated("cursor_grep", &count_args, temp.path());
        let parsed = result("cursor_grep", &count_args, &structured).expect("count result");
        assert_eq!(parsed["counts"][0]["count"], 1);

        std::fs::create_dir(temp.path().join("child")).expect("child directory");
        let ls_args = json!({"path":temp.path().to_string_lossy(), "ignore":["ignored.txt"]});
        let structured = run_generated("cursor_ls", &ls_args, temp.path());
        let parsed = result("cursor_ls", &ls_args, &structured).expect("ls result");
        assert!(
            !parsed["files"]
                .as_array()
                .unwrap()
                .contains(&json!("ignored.txt"))
        );
        assert!(parsed["directories"][0].as_str().unwrap().starts_with('/'));
    }
}
