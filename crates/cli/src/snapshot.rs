use colored::Colorize;
use perpl_sdk::state::Exchange;

pub(crate) fn render(exchange: Exchange) {
    println!(
        "{}\n",
        format!("{:#^144}", " Perpl Exchange Snapshot ")
            .bold()
            .purple()
    );
    println!("{:#}", exchange);
}
