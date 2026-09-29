//! Code templates, global scripts and configuration map values: the
//! server-wide parts of an export that channels refer to.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;

use regex::Regex;

use crate::xml::Element;

/// How Mirth uses a code template.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TemplateKind {
    /// Functions, available to scripts that call them.
    Function,
    /// Code Mirth adds to every script in the template's contexts.
    Compiled,
    /// A snippet the editor inserts; never part of a running script.
    Snippet,
}

/// One code template.
#[derive(Debug, Clone)]
pub(crate) struct CodeTemplate {
    pub(crate) name: String,
    pub(crate) library: String,
    pub(crate) code: String,
    pub(crate) kind: TemplateKind,
    /// The functions the template declares.
    functions: Vec<String>,
}

/// A code template library and the channels it applies to.
#[derive(Debug, Clone)]
pub(crate) struct Library {
    id: Option<String>,
    pub(crate) name: String,
    pub(crate) location: String,
    include_new: bool,
    enabled: BTreeSet<String>,
    disabled: BTreeSet<String>,
    /// Whether the export lists channels for the library at all.
    scoped: bool,
    pub(crate) templates: Vec<CodeTemplate>,
}

impl Library {
    fn applies_to(&self, channel: Option<&str>) -> bool {
        if !self.scoped {
            return true;
        }
        match channel {
            Some(id) if self.disabled.contains(id) => false,
            Some(id) if self.enabled.contains(id) => true,
            _ => self.include_new,
        }
    }
}

/// A server-wide script.
#[derive(Debug, Clone)]
pub(crate) struct GlobalScript {
    pub(crate) name: String,
    pub(crate) body: String,
    pub(crate) location: String,
}

/// Everything server-wide that channels refer to.
#[derive(Debug, Clone, Default)]
pub(crate) struct Globals {
    pub(crate) libraries: Vec<Library>,
    pub(crate) scripts: Vec<GlobalScript>,
    /// Configuration map values from the export.
    pub(crate) configuration: BTreeMap<String, String>,
    /// Values supplied by the caller; they take precedence.
    pub(crate) overrides: BTreeMap<String, String>,
    /// The location of the configuration map, when the export has one.
    pub(crate) configuration_location: Option<String>,
}

static FUNCTION: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"\bfunction\s+([A-Za-z_$][\w$]*)\s*\(").ok());
static CALL: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"(?:^|[^\w$.])([A-Za-z_$][\w$]*)\s*\(").ok());
static PLACEHOLDER: LazyLock<Option<Regex>> = LazyLock::new(|| Regex::new(r"\$\{([^{}]+)\}").ok());

fn declared_functions(code: &str) -> Vec<String> {
    FUNCTION.as_ref().map_or_else(Vec::new, |regex| {
        regex
            .captures_iter(code)
            .filter_map(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
            .collect()
    })
}

fn called_names(code: &str) -> BTreeSet<String> {
    CALL.as_ref().map_or_else(BTreeSet::new, |regex| {
        regex
            .captures_iter(code)
            .filter_map(|c| c.get(1))
            .map(|m| m.as_str().to_owned())
            .collect()
    })
}

/// Line endings as `\n`.
pub(crate) fn normalize_newlines(text: &str) -> String {
    text.replace("\r\n", "\n").replace('\r', "\n")
}

/// Whether a script does nothing beyond Mirth's default body (`return;`,
/// `return message;`) and comments.
pub(crate) fn is_trivial(script: &str) -> bool {
    let mut code = String::new();
    let mut rest = script;
    while !rest.is_empty() {
        if let Some(after) = rest.strip_prefix("//") {
            rest = after.find('\n').map_or("", |end| &after[end..]);
        } else if let Some(after) = rest.strip_prefix("/*") {
            rest = after.find("*/").map_or("", |end| &after[end + 2..]);
        } else {
            let mut chars = rest.chars();
            if let Some(c) = chars.next()
                && !c.is_whitespace()
            {
                code.push(c);
            }
            rest = chars.as_str();
        }
    }
    matches!(
        code.as_str(),
        "" | "return;" | "returnmessage;" | "returntrue;"
    )
}

impl Globals {
    /// Collects the libraries, global scripts and configuration map found
    /// anywhere in `root`.
    pub(crate) fn collect(&mut self, root: &Element) {
        match root.name.as_str() {
            "codeTemplateLibrary" => self.add_library(root),
            "codeTemplate" => self.add_loose_template(root),
            "globalScripts" => self.add_scripts(root),
            "configurationMap" => self.add_configuration(root),
            _ => {
                for child in &root.children {
                    self.collect(child);
                }
            }
        }
    }

    fn add_library(&mut self, element: &Element) {
        let id = element.value("id").map(str::to_owned);
        if id.is_some() && self.libraries.iter().any(|library| library.id == id) {
            return;
        }
        let list = |name: &str| -> BTreeSet<String> {
            element
                .child(name)
                .map(|list| {
                    list.strings()
                        .into_iter()
                        .map(|s| s.trim().to_owned())
                        .collect()
                })
                .unwrap_or_default()
        };
        let name = element
            .value("name")
            .unwrap_or("unnamed library")
            .to_owned();
        let templates = element
            .child("codeTemplates")
            .map(|list| {
                list.children_named("codeTemplate")
                    .filter_map(|template| template_of(template, &name))
                    .collect()
            })
            .unwrap_or_default();
        self.libraries.push(Library {
            id,
            name,
            location: element.location.clone(),
            include_new: element.flag("includeNewChannels").unwrap_or(true),
            enabled: list("enabledChannelIds"),
            disabled: list("disabledChannelIds"),
            scoped: element.child("enabledChannelIds").is_some()
                || element.child("includeNewChannels").is_some(),
            templates,
        });
    }

    /// A code template outside a library (Mirth 3.0 to 3.2 exports).
    fn add_loose_template(&mut self, element: &Element) {
        let Some(template) = template_of(element, "code templates") else {
            return;
        };
        match self
            .libraries
            .iter_mut()
            .find(|library| library.id.as_deref() == Some("loose-code-templates"))
        {
            Some(library) => library.templates.push(template),
            None => self.libraries.push(Library {
                id: Some("loose-code-templates".into()),
                name: "code templates".into(),
                location: element.location.clone(),
                include_new: true,
                enabled: BTreeSet::new(),
                disabled: BTreeSet::new(),
                scoped: false,
                templates: vec![template],
            }),
        }
    }

    fn add_scripts(&mut self, element: &Element) {
        for entry in element.children_named("entry") {
            let strings: Vec<&Element> = entry.children_named("string").collect();
            if let [name, body] = strings.as_slice() {
                self.scripts.push(GlobalScript {
                    name: name.raw_text().trim().to_owned(),
                    body: normalize_newlines(body.raw_text()),
                    location: entry.location.clone(),
                });
            }
        }
    }

    fn add_configuration(&mut self, element: &Element) {
        self.configuration_location
            .get_or_insert_with(|| element.location.clone());
        for entry in element.children_named("entry") {
            let Some(key) = entry
                .child("string")
                .map(|k| k.raw_text().trim().to_owned())
            else {
                continue;
            };
            let value = entry
                .children
                .iter()
                .skip(1)
                .find_map(|value| {
                    if value.name == "string" {
                        Some(value.raw_text().to_owned())
                    } else {
                        value.script("value").map(str::to_owned)
                    }
                })
                .unwrap_or_default();
            self.configuration.entry(key).or_insert(value);
        }
    }

    /// The templates available to the channel with Mirth identifier `id`.
    pub(crate) fn templates_for(&self, channel: Option<&str>) -> Vec<&CodeTemplate> {
        self.libraries
            .iter()
            .filter(|library| library.applies_to(channel))
            .flat_map(|library| library.templates.iter())
            .collect()
    }

    /// Replaces `${name}` placeholders with configuration map values.
    /// Returns the text and the names that have no value.
    pub(crate) fn resolve(&self, text: &str) -> (String, Vec<String>) {
        let Some(regex) = PLACEHOLDER.as_ref() else {
            return (text.to_owned(), Vec::new());
        };
        let mut missing = Vec::new();
        let resolved = regex.replace_all(text, |caps: &regex::Captures<'_>| {
            let name = caps.get(1).map_or("", |m| m.as_str());
            match self
                .overrides
                .get(name)
                .or_else(|| self.configuration.get(name))
            {
                Some(value) => value.clone(),
                None => {
                    missing.push(name.to_owned());
                    caps.get(0).map_or("", |m| m.as_str()).to_owned()
                }
            }
        });
        (resolved.into_owned(), missing)
    }
}

fn template_of(element: &Element, library: &str) -> Option<CodeTemplate> {
    let name = element
        .value("name")
        .unwrap_or("unnamed template")
        .to_owned();
    let properties = element.child("properties");
    let code = properties
        .and_then(|p| p.script("code"))
        .or_else(|| element.script("code"))?;
    let code = normalize_newlines(code);
    let kind = properties
        .and_then(|p| p.value("type"))
        .or_else(|| element.value("type"))
        .unwrap_or("FUNCTION");
    let kind = match kind {
        "FUNCTION" => TemplateKind::Function,
        "COMPILED_CODE" => TemplateKind::Compiled,
        _ => TemplateKind::Snippet,
    };
    Some(CodeTemplate {
        functions: declared_functions(&code),
        name,
        library: library.to_owned(),
        code,
        kind,
    })
}

/// Prepends the templates a script needs: every compiled-code template and
/// the function templates it calls, directly or through other templates.
/// Returns the script and the names of the templates included.
pub(crate) fn with_templates(script: &str, templates: &[&CodeTemplate]) -> (String, Vec<String>) {
    let mut included = vec![false; templates.len()];
    for (index, template) in templates.iter().enumerate() {
        included[index] = template.kind == TemplateKind::Compiled;
    }
    let mut pending: Vec<String> = called_names(script).into_iter().collect();
    for template in templates
        .iter()
        .filter(|t| t.kind == TemplateKind::Compiled)
    {
        pending.extend(called_names(&template.code));
    }
    let mut seen = BTreeSet::new();
    while let Some(name) = pending.pop() {
        if !seen.insert(name.clone()) {
            continue;
        }
        for (index, template) in templates.iter().enumerate() {
            if !included[index]
                && template.kind == TemplateKind::Function
                && template.functions.contains(&name)
            {
                included[index] = true;
                pending.extend(called_names(&template.code));
            }
        }
    }
    if !included.contains(&true) {
        return (script.to_owned(), Vec::new());
    }
    let mut out = String::new();
    let mut names = Vec::new();
    for (template, _) in templates
        .iter()
        .zip(&included)
        .filter(|(_, included)| **included)
    {
        out.push_str(&format!(
            "// Code template \"{}\" (library \"{}\")\n",
            template.name, template.library
        ));
        out.push_str(template.code.trim_end());
        out.push_str("\n\n");
        names.push(template.name.clone());
    }
    out.push_str(script);
    (out, names)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::xml::parse;

    #[test]
    fn includes_the_templates_a_script_calls() {
        let root = parse(
            "<serverConfiguration><codeTemplateLibraries><codeTemplateLibrary>\
             <id>L1</id><name>Utils</name><includeNewChannels>false</includeNewChannels>\
             <enabledChannelIds><string>C1</string></enabledChannelIds><disabledChannelIds/>\
             <codeTemplates>\
             <codeTemplate><name>pad</name><properties><type>FUNCTION</type>\
             <code>function pad(v) { return trimAll(v) + ' '; }</code></properties></codeTemplate>\
             <codeTemplate><name>trim</name><properties><type>FUNCTION</type>\
             <code>function trimAll(v) { return v.trim(); }</code></properties></codeTemplate>\
             <codeTemplate><name>unused</name><properties><type>FUNCTION</type>\
             <code>function unused() {}</code></properties></codeTemplate>\
             <codeTemplate><name>snippet</name><properties><type>DRAG_AND_DROP_CODE</type>\
             <code>pad(x);</code></properties></codeTemplate>\
             </codeTemplates></codeTemplateLibrary></codeTemplateLibraries>\
             <globalScripts><entry><string>Deploy</string><string>// comment\nreturn;</string></entry>\
             <entry><string>Preprocessor</string><string>logger.info(message);\nreturn message;</string></entry></globalScripts>\
             <configurationMap><entry><string>port</string>\
             <com.mirth.connect.util.ConfigurationProperty><value>6661</value><comment/></com.mirth.connect.util.ConfigurationProperty>\
             </entry></configurationMap></serverConfiguration>",
        )
        .unwrap();
        let mut globals = Globals::default();
        globals.collect(&root);
        assert!(globals.templates_for(Some("C2")).is_empty());
        let templates = globals.templates_for(Some("C1"));
        assert_eq!(templates.len(), 4);
        let (script, used) = with_templates("msg['PID'] = pad(x);", &templates);
        assert_eq!(used, ["pad", "trim"]);
        assert!(script.starts_with("// Code template \"pad\" (library \"Utils\")\nfunction pad"));
        assert!(script.ends_with("\n\nmsg['PID'] = pad(x);"));
        let (script, used) = with_templates("obj.pad(x);", &templates);
        assert!(used.is_empty());
        assert_eq!(script, "obj.pad(x);");

        assert!(is_trivial(&globals.scripts[0].body));
        assert!(!is_trivial(&globals.scripts[1].body));
        assert_eq!(
            globals.resolve("0.0.0.0:${port}"),
            ("0.0.0.0:6661".into(), vec![])
        );
        assert_eq!(
            globals.resolve("${dir}/in"),
            ("${dir}/in".into(), vec!["dir".to_owned()])
        );
    }
}
