//! `connect` subcommand: verify connectivity and print server metadata.

use clap::Args;

use crate::cli::ConnectionArgs;
use crate::error::Result;
use crate::opcua::client::shorten_policy_uri;
use crate::opcua::{Connection, NamespaceTable};

/// Arguments for the `connect` command.
#[derive(Debug, Args)]
pub struct ConnectArgs {
    /// Connection / authentication options.
    #[command(flatten)]
    pub connection: ConnectionArgs,
}

/// Run the `connect` command.
pub async fn run(args: ConnectArgs) -> Result<()> {
    let conn = Connection::connect(&args.connection).await?;

    let info = conn.server_info().await?;
    let namespaces = NamespaceTable::read(conn.session()).await?;

    println!("Connected to {}", conn.endpoint());
    println!();
    println!("Server");
    println!("  application name : {}", info.application_name);
    println!("  application URI  : {}", info.application_uri);
    println!("  product URI      : {}", info.product_uri);

    println!();
    println!("Namespaces");
    for (i, uri) in namespaces.uris().iter().enumerate() {
        println!("  [{i}] {uri}");
    }

    println!();
    println!("Endpoints");
    for ep in &info.endpoints {
        println!("  {}", ep.url);
        println!(
            "      security : {} / {} (level {})",
            shorten_policy_uri(&ep.security_policy),
            ep.security_mode,
            ep.security_level
        );
        if !ep.user_tokens.is_empty() {
            println!("      tokens   : {}", ep.user_tokens.join(", "));
        }
    }

    println!();
    println!("Certificate trust: {}", trust_status(&args.connection));

    conn.disconnect().await;
    Ok(())
}

fn trust_status(args: &ConnectionArgs) -> &'static str {
    if args.trust_server {
        "auto-trust enabled (--trust-server)"
    } else {
        "manual (untrusted server certs are placed in pki/rejected)"
    }
}
