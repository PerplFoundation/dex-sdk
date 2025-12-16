//! Book listener binary - listens to an order book on testnet and prints it.

use std::time::Duration;

use alloy::{
    providers::ProviderBuilder,
    rpc::client::RpcClient,
    transports::layers::RetryBackoffLayer,
};
use clap::{Parser, ValueEnum};
use dex_sdk::{
    Chain,
    state::{L2Book, L3Order, Perpetual, SnapshotBuilder},
    stream,
    types::{OrderType, PerpetualId, StateInstant},
};
use futures::StreamExt;

#[derive(Debug, Clone, Copy, ValueEnum, Default)]
enum DisplayMode {
    /// L2 view: aggregated price levels only
    L2,
    /// L3 view: show individual orders at each level
    #[default]
    L3,
    /// Compact L3: show orders in a condensed format
    Compact,
}

#[derive(Parser, Debug)]
#[command(name = "book_listener")]
#[command(about = "Listen to an order book on testnet and print it")]
struct Args {
    /// Chain to connect to (testnet or custom chain ID)
    #[arg(short, long, default_value = "testnet")]
    chain: String,

    /// Perpetual market ID to listen to
    #[arg(short, long)]
    market: PerpetualId,

    /// RPC URL to connect to
    #[arg(short, long)]
    rpc_url: String,

    /// Number of price levels to display (0 = all)
    #[arg(short, long, default_value = "10")]
    depth: usize,

    /// Poll interval in milliseconds
    #[arg(short, long, default_value = "500")]
    poll_interval: u64,

    /// Display mode: l2, l3, or compact
    #[arg(long, value_enum, default_value = "l3")]
    mode: DisplayMode,

    /// Maximum orders to show per level in L3 mode (0 = all)
    #[arg(long, default_value = "5")]
    orders_per_level: usize,
}

fn order_type_symbol(ot: OrderType) -> &'static str {
    match ot {
        OrderType::OpenLong => "OL",
        OrderType::OpenShort => "OS",
        OrderType::CloseLong => "CL",
        OrderType::CloseShort => "CS",
    }
}

fn print_order_compact(order: &L3Order) -> String {
    format!(
        "[#{:<5} acc:{:<6} sz:{:<12} {}]",
        order.order_id(),
        order.account_id(),
        format!("{}", order.order().size()),
        order_type_symbol(order.r#type()),
    )
}

fn print_order_detailed(order: &L3Order, current_block: u64) {
    let o = order.order();
    let expiry_str = if o.expiry_block() == 0 {
        "never".to_string()
    } else if o.expiry_block() <= current_block {
        format!("{} (EXPIRED)", o.expiry_block())
    } else {
        format!("{} (+{})", o.expiry_block(), o.expiry_block() - current_block)
    };

    println!(
        "       │ Order #{:<5} │ Acc: {:<6} │ Size: {:<14} │ Lev: {:<5} │ {} │ Exp: {}",
        order.order_id(),
        order.account_id(),
        format!("{}", o.size()),
        format!("{}x", o.leverage()),
        order_type_symbol(order.r#type()),
        expiry_str,
    );
}

fn print_l2_book(book: &L2Book, depth: usize) {
    println!("\n{:=^80}", " ORDER BOOK (L2) ");

    // Print asks (reversed so lowest ask is closest to spread)
    let asks: Vec<_> = book.asks().iter().collect();
    let ask_count = if depth == 0 { asks.len() } else { depth.min(asks.len()) };

    println!("{:^80}", "ASKS");
    println!("{:-^80}", "");
    println!(
        "{:>25} │ {:<25} │ {:<10} │ {:<10}",
        "Price", "Total Size", "Orders", "Cumulative"
    );
    println!("{:-^80}", "");

    let mut cumulative = fastnum::UD64::ZERO;
    let ask_slice: Vec<_> = asks.iter().take(ask_count).collect();
    for (price, level) in ask_slice.iter().rev() {
        cumulative += level.size();
        println!(
            "{:>25} │ {:<25} │ {:<10} │ {:<10}",
            format!("{}", price),
            format!("{}", level.size()),
            level.num_orders(),
            format!("{}", cumulative),
        );
    }

    // Print spread
    print_spread(book);

    // Print bids
    let bids: Vec<_> = book.bids().iter().collect();
    let bid_count = if depth == 0 { bids.len() } else { depth.min(bids.len()) };

    println!("{:^80}", "BIDS");
    println!("{:-^80}", "");
    println!(
        "{:>25} │ {:<25} │ {:<10} │ {:<10}",
        "Price", "Total Size", "Orders", "Cumulative"
    );
    println!("{:-^80}", "");

    cumulative = fastnum::UD64::ZERO;
    for (price, level) in bids.iter().take(bid_count) {
        cumulative += level.size();
        println!(
            "{:>25} │ {:<25} │ {:<10} │ {:<10}",
            format!("{}", price.0),
            format!("{}", level.size()),
            level.num_orders(),
            format!("{}", cumulative),
        );
    }

    print_summary(book);
}

fn print_l3_book(book: &L2Book, depth: usize, orders_per_level: usize, current_block: u64) {
    println!("\n{:=^100}", " ORDER BOOK (L3) ");

    // Print asks (reversed so lowest ask is closest to spread)
    let asks: Vec<_> = book.asks().iter().collect();
    let ask_count = if depth == 0 { asks.len() } else { depth.min(asks.len()) };

    println!("{:^100}", "ASKS");
    println!("{:-^100}", "");

    let ask_slice: Vec<_> = asks.iter().take(ask_count).collect();
    for (price, level) in ask_slice.iter().rev() {
        println!(
            "  ┌─ Price: {:<20} │ Total: {:<15} │ Orders: {}",
            format!("{}", price),
            format!("{}", level.size()),
            level.num_orders(),
        );

        // Get orders at this level via the book's ask_orders iterator filtered by price
        let level_orders: Vec<_> = book
            .ask_orders()
            .filter(|o| o.price() == **price)
            .collect();

        let show_count = if orders_per_level == 0 {
            level_orders.len()
        } else {
            orders_per_level.min(level_orders.len())
        };

        for order in level_orders.iter().take(show_count) {
            print_order_detailed(order, current_block);
        }

        if level_orders.len() > show_count {
            println!(
                "       │ ... and {} more orders",
                level_orders.len() - show_count
            );
        }
        println!("  └{:─^96}", "");
    }

    // Print spread
    print_spread(book);

    // Print bids
    let bids: Vec<_> = book.bids().iter().collect();
    let bid_count = if depth == 0 { bids.len() } else { depth.min(bids.len()) };

    println!("{:^100}", "BIDS");
    println!("{:-^100}", "");

    for (price, level) in bids.iter().take(bid_count) {
        println!(
            "  ┌─ Price: {:<20} │ Total: {:<15} │ Orders: {}",
            format!("{}", price.0),
            format!("{}", level.size()),
            level.num_orders(),
        );

        // Get orders at this level
        let level_orders: Vec<_> = book
            .bid_orders()
            .filter(|o| o.price() == price.0)
            .collect();

        let show_count = if orders_per_level == 0 {
            level_orders.len()
        } else {
            orders_per_level.min(level_orders.len())
        };

        for order in level_orders.iter().take(show_count) {
            print_order_detailed(order, current_block);
        }

        if level_orders.len() > show_count {
            println!(
                "       │ ... and {} more orders",
                level_orders.len() - show_count
            );
        }
        println!("  └{:─^96}", "");
    }

    print_summary(book);
}

fn print_compact_book(book: &L2Book, depth: usize, orders_per_level: usize) {
    println!("\n{:=^120}", " ORDER BOOK (Compact L3) ");

    // Print asks
    let asks: Vec<_> = book.asks().iter().collect();
    let ask_count = if depth == 0 { asks.len() } else { depth.min(asks.len()) };

    println!("{:^120}", "ASKS");
    println!("{:-^120}", "");

    let ask_slice: Vec<_> = asks.iter().take(ask_count).collect();
    for (price, level) in ask_slice.iter().rev() {
        let level_orders: Vec<_> = book
            .ask_orders()
            .filter(|o| o.price() == **price)
            .collect();

        let show_count = if orders_per_level == 0 {
            level_orders.len()
        } else {
            orders_per_level.min(level_orders.len())
        };

        let orders_str: Vec<_> = level_orders
            .iter()
            .take(show_count)
            .map(|o| print_order_compact(o))
            .collect();

        let more = if level_orders.len() > show_count {
            format!(" +{} more", level_orders.len() - show_count)
        } else {
            String::new()
        };

        println!(
            "{:>18} ({:>3}) │ {}{}",
            format!("{}", price),
            level.num_orders(),
            orders_str.join(" "),
            more,
        );
    }

    // Print spread
    print_spread(book);

    // Print bids
    let bids: Vec<_> = book.bids().iter().collect();
    let bid_count = if depth == 0 { bids.len() } else { depth.min(bids.len()) };

    println!("{:^120}", "BIDS");
    println!("{:-^120}", "");

    for (price, level) in bids.iter().take(bid_count) {
        let level_orders: Vec<_> = book
            .bid_orders()
            .filter(|o| o.price() == price.0)
            .collect();

        let show_count = if orders_per_level == 0 {
            level_orders.len()
        } else {
            orders_per_level.min(level_orders.len())
        };

        let orders_str: Vec<_> = level_orders
            .iter()
            .take(show_count)
            .map(|o| print_order_compact(o))
            .collect();

        let more = if level_orders.len() > show_count {
            format!(" +{} more", level_orders.len() - show_count)
        } else {
            String::new()
        };

        println!(
            "{:>18} ({:>3}) │ {}{}",
            format!("{}", price.0),
            level.num_orders(),
            orders_str.join(" "),
            more,
        );
    }

    print_summary(book);
}

fn print_spread(book: &L2Book) {
    let best_bid = book.best_bid();
    let best_ask = book.best_ask();
    if let (Some((bid_price, bid_size)), Some((ask_price, ask_size))) = (best_bid, best_ask) {
        let spread = ask_price - bid_price;
        let mid = (ask_price + bid_price) / fastnum::udec64!(2);
        let spread_bps = (spread / mid) * fastnum::udec64!(10000);
        println!(
            "{:=^100}",
            format!(
                " SPREAD: {} ({:.2} bps) | Best Bid: {} ({}) | Best Ask: {} ({}) ",
                spread, spread_bps, bid_price, bid_size, ask_price, ask_size
            )
        );
    } else {
        println!("{:=^100}", " NO SPREAD (empty side) ");
    }
}

fn print_summary(book: &L2Book) {
    println!("{:=^100}", "");

    // Calculate total sizes
    let total_ask_size: fastnum::UD64 = book.asks().values().map(|l| l.size()).sum();
    let total_bid_size: fastnum::UD64 = book.bids().values().map(|l| l.size()).sum();

    println!(
        "Total: {} orders │ Asks: {} levels, {} size │ Bids: {} levels, {} size",
        book.total_orders(),
        book.asks().len(),
        total_ask_size,
        book.bids().len(),
        total_bid_size,
    );

    // Imbalance
    if total_ask_size > fastnum::UD64::ZERO || total_bid_size > fastnum::UD64::ZERO {
        let total = total_ask_size + total_bid_size;
        let bid_pct = (total_bid_size / total) * fastnum::udec64!(100);
        let ask_pct = (total_ask_size / total) * fastnum::udec64!(100);
        println!(
            "Imbalance: {:.1}% bids / {:.1}% asks",
            bid_pct, ask_pct
        );
    }
}

fn print_book(book: &L2Book, mode: DisplayMode, depth: usize, orders_per_level: usize, current_block: u64) {
    match mode {
        DisplayMode::L2 => print_l2_book(book, depth),
        DisplayMode::L3 => print_l3_book(book, depth, orders_per_level, current_block),
        DisplayMode::Compact => print_compact_book(book, depth, orders_per_level),
    }
}

fn print_market_info(perp: &Perpetual) {
    println!("\n{:=^80}", " MARKET INFO ");
    println!("Name:            {} ({})", perp.name(), perp.symbol());
    println!("Perpetual ID:    {}", perp.id());
    println!("Last Price:      {}", perp.last_price());
    println!("Mark Price:      {}", perp.mark_price());
    println!("Oracle Price:    {}", perp.oracle_price());
    println!("Funding Rate:    {}", perp.funding_rate());
    println!("Open Interest:   {}", perp.open_interest());
    println!("Maker Fee:       {}", perp.maker_fee());
    println!("Taker Fee:       {}", perp.taker_fee());
    println!("Init Margin:     {}", perp.initial_margin());
    println!("Maint Margin:    {}", perp.maintenance_margin());
    println!("Paused:          {}", perp.is_paused());
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    let args = Args::parse();

    // Build chain configuration
    let chain = match args.chain.as_str() {
        "testnet" => Chain::testnet(),
        _ => {
            eprintln!("Only 'testnet' is currently supported for chain");
            std::process::exit(1);
        }
    };

    // Check if the market is valid for this chain
    if !chain.perpetuals().contains(&args.market) {
        eprintln!(
            "Market {} is not available on this chain. Available markets: {:?}",
            args.market,
            chain.perpetuals()
        );
        std::process::exit(1);
    }

    println!("Connecting to {} ...", args.rpc_url);

    // Build RPC client with retry layer
    let client = RpcClient::builder()
        .layer(RetryBackoffLayer::new(10, 100, 200))
        .connect(&args.rpc_url)
        .await?;
    client.set_poll_interval(Duration::from_millis(args.poll_interval));
    let provider = ProviderBuilder::new().connect_client(client);

    println!("Building initial snapshot for market {} ...", args.market);

    // Build initial snapshot
    let mut exchange = SnapshotBuilder::new(&chain, provider.clone())
        .with_perpetuals(vec![args.market])
        .build()
        .await?;

    let instant = exchange.instant();
    println!(
        "Snapshot built at block {} (timestamp: {})",
        instant.block_number(),
        instant.block_timestamp()
    );

    // Print initial book state
    if let Some(perp) = exchange.perpetuals().get(&args.market) {
        print_market_info(perp);
        print_book(
            perp.l2_book(),
            args.mode,
            args.depth,
            args.orders_per_level,
            instant.block_number(),
        );
    }

    println!("\nListening for updates... (Ctrl+C to stop)");

    // Stream events and update the book
    let mut event_stream = Box::pin(stream::raw(
        &chain,
        provider,
        StateInstant::new(instant.block_number() + 1, 0),
        tokio::time::sleep,
    ));

    while let Some(result) = event_stream.next().await {
        match result {
            Ok(block_events) => {
                let block_num = block_events.instant().block_number();
                let event_count = block_events.events().len();

                // Apply events to update state
                match exchange.apply_events(&block_events) {
                    Ok(Some(state_events)) => {
                        let state_event_count: usize = state_events
                            .events()
                            .iter()
                            .map(|e| e.event().len())
                            .sum();

                        if state_event_count > 0 {
                            println!(
                                "\n{:=^80}",
                                format!(" BLOCK {} | {} raw -> {} state events ", block_num, event_count, state_event_count)
                            );

                            // Print updated book
                            if let Some(perp) = exchange.perpetuals().get(&args.market) {
                                println!(
                                    "Last: {} | Mark: {} | Oracle: {}",
                                    perp.last_price(),
                                    perp.mark_price(),
                                    perp.oracle_price()
                                );
                                print_book(
                                    perp.l2_book(),
                                    args.mode,
                                    args.depth,
                                    args.orders_per_level,
                                    block_num,
                                );
                            }
                        }
                    }
                    Ok(None) => {
                        // Block already applied, skip
                    }
                    Err(e) => {
                        eprintln!("Error applying events: {:?}", e);
                    }
                }
            }
            Err(e) => {
                eprintln!("Error fetching events: {:?}", e);
            }
        }
    }

    Ok(())
}
