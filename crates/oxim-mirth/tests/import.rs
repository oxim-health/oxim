//! Imports synthetic Mirth Connect exports and compares the channel files
//! and reports with golden files. Set `OXIM_BLESS=1` to rewrite them.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::path::{Path, PathBuf};

use oxim_core::ChannelConfig;
use oxim_mirth::{ImportOptions, ImportResult, MigrationReport, Outcome, import};

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("tests")
}

fn read(name: &str) -> String {
    std::fs::read_to_string(fixtures().join("fixtures").join(name)).unwrap()
}

/// Compares `actual` with the golden file, or rewrites it when blessing.
fn golden(dir: &str, file: &str, actual: &str) {
    let path = fixtures().join("golden").join(dir).join(file);
    if std::env::var_os("OXIM_BLESS").is_some() {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(&path, actual).unwrap();
        return;
    }
    let expected = std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("{}: {e}; run with OXIM_BLESS=1", path.display()));
    assert_eq!(
        actual.replace("\r\n", "\n"),
        expected.replace("\r\n", "\n"),
        "{} differs; run with OXIM_BLESS=1 to accept",
        path.display()
    );
}

fn check(dir: &str, result: &ImportResult) {
    for channel in &result.channels {
        let config = ChannelConfig::from_yaml(&channel.yaml)
            .unwrap_or_else(|e| panic!("{}: {e}\n{}", channel.file_name, channel.yaml));
        assert_eq!(config.id.as_str(), channel.id);
        assert_eq!(config.enabled, channel.enabled);
        golden(dir, &channel.file_name, &channel.yaml);
    }
    golden(dir, "report.md", &result.report.to_markdown());
}

fn detail<'a>(report: &'a MigrationReport, element: &str) -> Vec<(&'a str, Outcome)> {
    report
        .items
        .iter()
        .filter(|item| item.element.contains(element))
        .map(|item| (item.detail.as_str(), item.outcome))
        .collect()
}

#[test]
fn imports_a_channel_with_declarative_filters_and_mappings() {
    let options = ImportOptions::new()
        .with_value("mllp_port", "6661")
        .with_value("archive_dir", "/var/lib/oxim/archive");
    let result = import(&read("adt-archive.xml"), &options).unwrap();
    assert_eq!(result.channels.len(), 1);
    let channel = &result.channels[0];
    assert_eq!(channel.id, "adt-archive");
    assert_eq!(channel.file_name, "adt-archive.yaml");
    assert!(channel.enabled && !channel.draft);
    let report = &result.report;
    assert_eq!(report.export, "channel export (Mirth Connect 3.12.0)");
    assert_eq!(
        report.count(Outcome::Unsupported),
        0,
        "{}",
        report.to_markdown()
    );
    assert_eq!(
        report.count(Outcome::Approximated),
        0,
        "{}",
        report.to_markdown()
    );
    let config = ChannelConfig::from_yaml(&channel.yaml).unwrap();
    assert_eq!(config.filters.len(), 3);
    assert_eq!(config.filters[0].kind, "path-in");
    assert_eq!(config.transformers.len(), 1);
    assert_eq!(config.transformers[0].kind, "map");
    assert_eq!(config.destinations.len(), 1);
    check("adt-archive", &result);
    assert!(!channel.yaml.contains("type: script"), "{}", channel.yaml);
}

#[test]
fn imports_scripts_code_templates_and_reports_what_is_missing() {
    let result = import(&read("lab-results.xml"), &ImportOptions::new()).unwrap();
    let channel = &result.channels[0];
    assert_eq!(channel.id, "lab-results-analyzer-lis");
    // The channel starts stopped in Mirth.
    assert!(!channel.enabled);
    let config = ChannelConfig::from_yaml(&channel.yaml).unwrap();
    // ORU OR JavaScript: one combined script filter.
    assert_eq!(config.filters.len(), 1);
    assert_eq!(config.filters[0].kind, "script");
    let script = config.filters[0].settings["source"].as_str().unwrap();
    assert!(script.contains("return rule1() || rule2();"), "{script}");
    // The JavaScript step brings the templates it calls, and only those.
    let step = config.transformers[0].settings["source"].as_str().unwrap();
    assert!(step.contains("function formatName(value)"), "{step}");
    assert!(step.contains("function capitalize(value)"), "{step}");
    assert!(!step.contains("unusedHelper"), "{step}");
    assert!(
        step.contains("\tchannelMap.put('hasResults', 'yes');\n}"),
        "{step}"
    );
    // Credentials are never copied.
    assert!(!channel.yaml.contains("not-a-real-password"));
    assert!(!channel.yaml.contains("portal:"));
    let report = &result.report;
    assert_eq!(
        detail(report, "\"Stylesheet\""),
        [(
            "XSLT steps are not supported; the step was left out",
            Outcome::Unsupported
        )]
    );
    assert!(
        detail(report, "\"Warehouse\"")[0]
            .0
            .contains("Database Writer")
    );
    assert!(detail(report, "\"Route copy\"")[0].0.contains("router."));
    assert_eq!(detail(report, "\"Route copy\"")[0].1, Outcome::Approximated);
    assert_eq!(
        detail(report, "\"lastAnalyzer\"")[0].1,
        Outcome::Approximated
    );
    assert_eq!(
        detail(report, "response transformer")[0].1,
        Outcome::Unsupported
    );
    assert_eq!(
        detail(report, "channel preprocessor script")[0].1,
        Outcome::Unsupported
    );
    assert!(
        detail(report, "destination chain")[0]
            .0
            .contains("\"Portal\"")
    );
    assert!(
        detail(report, "source response (d1)")[0]
            .0
            .contains("not supported")
    );
    check("lab-results", &result);
}

#[test]
fn imports_a_server_configuration_backup() {
    let result = import(&read("server-config.xml"), &ImportOptions::new()).unwrap();
    let ids: Vec<&str> = result.channels.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["lab-feed", "lab-feed-2", "warehouse-import"]);
    let files: Vec<&str> = result
        .channels
        .iter()
        .map(|c| c.file_name.as_str())
        .collect();
    assert_eq!(
        files,
        [
            "lab-feed.yaml",
            "lab-feed-2.yaml",
            "warehouse-import.yaml.draft"
        ]
    );
    let feed = ChannelConfig::from_yaml(&result.channels[0].yaml).unwrap();
    assert_eq!(
        feed.source.settings["directory"].as_str(),
        Some("/var/lib/lab/inbox")
    );
    assert_eq!(
        feed.destinations[0].settings["url"].as_str(),
        Some("https://lis.example.org/api/results")
    );
    // The configured library applies to the first channel only.
    let filter = feed.filters[0].settings["source"].as_str().unwrap();
    assert!(filter.contains("function isValid(message)"), "{filter}");
    let second = ChannelConfig::from_yaml(&result.channels[1].yaml).unwrap();
    assert_eq!(second.filters[0].kind, "condition");
    let destinations: Vec<&str> = second.destinations.iter().map(|d| d.id.as_str()).collect();
    assert_eq!(destinations, ["destination-1", "destination-1-2"]);
    assert!(result.channels[2].draft && !result.channels[2].enabled);
    let report = &result.report;
    assert_eq!(report.channels.len(), 3);
    assert!(report.channels[2].draft);
    assert_eq!(
        detail(report, "global preprocessor script")[0].1,
        Outcome::Unsupported
    );
    assert!(detail(report, "global deploy script").is_empty());
    assert_eq!(detail(report, "alerts (1)")[0].1, Outcome::Unsupported);
    assert_eq!(detail(report, "users (1)")[0].1, Outcome::Unsupported);
    assert!(
        detail(report, "configuration map")[0]
            .0
            .starts_with("2 value(s)")
    );
    assert!(
        detail(report, "Channel Writer")[0]
            .0
            .contains("no destination connector")
    );
    let json: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
    assert_eq!(
        json["channels"][2]["file_name"],
        "warehouse-import.yaml.draft"
    );
    check("server-config", &result);
}

#[test]
fn imports_channel_groups_and_can_skip_disabled_channels() {
    let xml = read("channel-group.xml");
    let result = import(&xml, &ImportOptions::new()).unwrap();
    assert_eq!(
        result.report.export,
        "channel group export (Mirth Connect 3.12.0)"
    );
    let ids: Vec<&str> = result.channels.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids, ["glucose-meters", "retired-meters"]);
    assert!(result.channels[0].draft);
    assert!(!result.channels[1].enabled);
    check("channel-group", &result);

    let result = import(&xml, &ImportOptions::new().skip_disabled()).unwrap();
    assert_eq!(result.channels.len(), 1);
    assert!(
        result
            .report
            .items
            .iter()
            .any(|item| item.detail == "disabled in Mirth; skipped")
    );
}

#[test]
fn code_templates_can_come_from_another_export() {
    let channel = read("adt-archive.xml").replace(
        "<name>Old logging</name>\n          <sequenceNumber>4</sequenceNumber>\n          <enabled>false</enabled>",
        "<name>Old logging</name>\n          <sequenceNumber>4</sequenceNumber>\n          <enabled>true</enabled>",
    );
    let channel = channel.replace("logger.info('received');", "audit(msg);");
    let library = r#"<codeTemplateLibrary version="3.12.0">
        <id>lib</id><name>Audit</name>
        <codeTemplates><codeTemplate><name>audit</name>
        <properties><type>FUNCTION</type><code>function audit(m) { logger.info(m); }</code></properties>
        </codeTemplate></codeTemplates></codeTemplateLibrary>"#;
    let options = ImportOptions::new()
        .with_library(library)
        .with_value("mllp_port", "6661")
        .with_value("archive_dir", "archive");
    let result = import(&channel, &options).unwrap();
    let yaml = &result.channels[0].yaml;
    assert!(yaml.contains("function audit(m)"), "{yaml}");
    assert!(
        result
            .report
            .items
            .iter()
            .any(|item| item.detail.contains("code templates included: audit"))
    );
}

#[test]
fn rejects_what_is_not_a_mirth_export() {
    assert!(matches!(
        import("<html><body/></html>", &ImportOptions::new()),
        Err(oxim_mirth::MirthError::NotAnExport(name)) if name == "html"
    ));
    assert!(matches!(
        import("<channel>", &ImportOptions::new()),
        Err(oxim_mirth::MirthError::Xml(_))
    ));
    let result = import(
        "<channel version=\"3.12.0\"><name>Refs only</name></channel>",
        &ImportOptions::new(),
    )
    .unwrap();
    assert!(result.channels.is_empty());
}

#[test]
fn unresolved_placeholders_are_reported() {
    let result = import(&read("adt-archive.xml"), &ImportOptions::new()).unwrap();
    let report = &result.report;
    let unresolved: Vec<_> = report
        .items
        .iter()
        .filter(|item| {
            item.detail
                .contains("refers to Mirth variables without a value")
        })
        .collect();
    assert_eq!(unresolved.len(), 2, "{}", report.to_markdown());
    assert!(result.channels[0].yaml.contains("${archive_dir}"));
}
