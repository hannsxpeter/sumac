fn main() {
    let path = std::env::current_exe().expect("resolve fixture executable");
    let name = path.file_stem().unwrap().to_str().unwrap();
    if !std::env::args().any(|arg| arg == "--help") {
        std::process::exit(2);
    }
    if name == "fixture-sparse" {
        println!("Usage: {name} [OPTIONS]");
        return;
    }
    println!(
        "Manage records in a local workspace.

Usage: {name} [OPTIONS] <COMMAND>

Commands:
  list    List records
  show    Display a record
  search  Find records by keyword

Options:
  --json  Write records as JSON
  --help  Print help

Examples:
  {name} list --json"
    );
}
