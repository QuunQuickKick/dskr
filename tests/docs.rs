use clap::CommandFactory;

#[test]
fn usage_documents_all_options() {
    let doc = include_str!("../USAGE.md");
    let cmd = dskr::cli::Cli::command();

    for arg in cmd.get_arguments() {
        if let Some(long) = arg.get_long() {
            let needle = format!("--{long}");
            assert!(
                doc.contains(&needle),
                "USAGE.md に {needle} の記載がありません"
            );
        }
    }
    assert!(doc.contains("PATH"), "USAGE.md に PATH の記載がありません");
}
