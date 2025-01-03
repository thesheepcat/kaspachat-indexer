use clap::Parser;
/// Parsing CLI flags
#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
pub(crate) struct Args {
    #[arg(short = 'a', long = "rusty-kaspa-address")]
    rusty_kaspa_address: String,
}
impl Args {
    pub(crate) fn rusty_kaspa_address(&self) -> String {
        self.rusty_kaspa_address.to_string()
    }

}