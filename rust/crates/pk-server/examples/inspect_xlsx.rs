use pk_server::admin::parse_any_text;
use pk_server::admin::spreadsheet_to_text;

fn main() -> anyhow::Result<()> {
    let path = std::env::args().nth(1).expect("usage: inspect_xlsx <file>");
    let bytes = std::fs::read(path)?;
    let text = spreadsheet_to_text(&bytes)?;
    println!("{text}");
    let rows = parse_any_text(&text);
    for (i, row) in rows.iter().enumerate() {
        println!(
            "{:02}: name={:?} douyin={:?} anchor={:?} wave={:?} minutes={:?}",
            i, row.name, row.douyin_no, row.anchor_id, row.wave, row.minutes
        );
    }
    Ok(())
}
