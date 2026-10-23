//! Redis requirepass parsing. Authentication values are never included in errors.
use crate::{Result, ensure};

fn space(byte: u8) -> bool {
    byte.is_ascii_whitespace() || byte == 0x0b
}

pub fn directive(line: &str) -> &str {
    line.trim_start_matches(|c: char| c.is_ascii() && space(c as u8))
        .split([' ', '\t', '\r', '\n'])
        .next()
        .unwrap_or("")
}

pub fn password(text: &str) -> Result<Vec<u8>> {
    let mut password = Vec::new();
    for (index, line) in text.lines().enumerate() {
        let line = line.trim_matches([' ', '\t', '\r', '\n']);
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let raw_key = directive(line);
        ensure!(
            !raw_key
                .split_once('=')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case("requirepass")),
            "第 {} 行的 requirepass 应使用空格分隔密码参数，不能使用等号",
            index + 1
        );
        // Quoted directive names can bypass scalar ownership checks and are not accepted.
        ensure!(
            !raw_key.contains(['\'', '"', '\\']) && !raw_key.bytes().any(|b| b.is_ascii_control()),
            "第 {} 行的 Redis 指令名称不能使用引号、转义或控制字符，请使用空格分隔参数",
            index + 1
        );
        if !raw_key.eq_ignore_ascii_case("requirepass") {
            continue;
        }
        let tokens = arguments(line.as_bytes()).map_err(|_| {
            crate::anyhow!(
                "第 {} 行的 requirepass 格式无效，请使用单个密码参数",
                index + 1
            )
        })?;
        ensure!(
            tokens.len() == 2 && tokens[1].len() <= 512 && !tokens[1].contains(&0),
            "第 {} 行的 requirepass 必须包含一个不超过 512 字节且不含空字符的参数",
            index + 1
        );
        password = tokens[1].clone();
    }
    Ok(password)
}

fn arguments(line: &[u8]) -> Result<Vec<Vec<u8>>> {
    let mut result = Vec::new();
    let mut i = 0;
    while i < line.len() {
        while i < line.len() && space(line[i]) {
            i += 1;
        }
        if i == line.len() {
            break;
        }
        let mut value = Vec::new();
        let mut quote = None;
        loop {
            if let Some(q) = quote {
                ensure!(i < line.len(), "未闭合引号");
                let c = line[i];
                i += 1;
                if c == q {
                    ensure!(i == line.len() || space(line[i]), "引号后需要空格");
                    break;
                }
                if c == b'\\' && i < line.len() && (q == b'"' || line[i] == b'\'') {
                    let escaped = line[i];
                    i += 1;
                    if q == b'"'
                        && escaped == b'x'
                        && i + 1 < line.len()
                        && line[i].is_ascii_hexdigit()
                        && line[i + 1].is_ascii_hexdigit()
                    {
                        let digit = |b: u8| (b as char).to_digit(16).unwrap_or(0) as u8;
                        value.push(digit(line[i]) * 16 + digit(line[i + 1]));
                        i += 2;
                    } else {
                        value.push(if q == b'"' {
                            match escaped {
                                b'n' => b'\n',
                                b'r' => b'\r',
                                b't' => b'\t',
                                b'b' => 8,
                                b'a' => 7,
                                other => other,
                            }
                        } else {
                            escaped
                        });
                    }
                } else {
                    value.push(c);
                }
            } else {
                if i == line.len() || matches!(line[i], b' ' | b'\t' | b'\r' | b'\n') {
                    break;
                }
                let c = line[i];
                i += 1;
                if matches!(c, b'"' | b'\'') {
                    quote = Some(c);
                } else {
                    value.push(c);
                }
            }
        }
        result.push(value);
    }
    Ok(result)
}
