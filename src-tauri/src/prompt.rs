use std::collections::HashSet;

pub(crate) fn prompt_text(source: &str) -> &str {
    source
        .strip_suffix("\r\n")
        .or_else(|| source.strip_suffix('\n'))
        .unwrap_or(source)
}

pub(crate) fn render_prompt_template(
    source: &str,
    values: &[(&str, &str)],
) -> Result<String, String> {
    let template = prompt_text(source);
    let mut supplied = HashSet::new();
    for (key, _) in values {
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
            || !supplied.insert(*key)
        {
            return Err(format!("Prompt 模板变量无效或重复：{key}"));
        }
    }

    let extra_capacity = values.iter().fold(0_usize, |total, (_, value)| {
        total.saturating_add(value.len())
    });
    let mut output = String::with_capacity(template.len().saturating_add(extra_capacity));
    let mut remaining = template;
    let mut used = HashSet::new();
    loop {
        let Some(opening) = remaining.find("{{") else {
            if remaining.contains("}}") {
                return Err("Prompt 模板包含未配对的结束标记".to_string());
            }
            output.push_str(remaining);
            break;
        };
        let prefix = &remaining[..opening];
        if prefix.contains("}}") {
            return Err("Prompt 模板包含未配对的结束标记".to_string());
        }
        output.push_str(prefix);
        let after_opening = &remaining[opening + 2..];
        let closing = after_opening
            .find("}}")
            .ok_or_else(|| "Prompt 模板包含未闭合变量".to_string())?;
        let key = &after_opening[..closing];
        if key.is_empty()
            || !key
                .bytes()
                .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_')
        {
            return Err(format!("Prompt 模板变量名无效：{key}"));
        }
        let value = values
            .iter()
            .find_map(|(candidate, value)| (*candidate == key).then_some(*value))
            .ok_or_else(|| format!("Prompt 模板缺少变量值：{key}"))?;
        output.push_str(value);
        used.insert(key);
        remaining = &after_opening[closing + 2..];
    }
    if used.len() != supplied.len() {
        return Err("Prompt 模板存在未使用的变量值".to_string());
    }
    Ok(output)
}
