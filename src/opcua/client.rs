//! Client / session construction and lifecycle.

use std::sync::Arc;
use std::time::Duration;

use opcua::client::{Client, ClientBuilder, IdentityToken, Session};
use opcua::crypto::SecurityPolicy;
use opcua::types::{
    ApplicationDescription, EndpointDescription, MessageSecurityMode, UserTokenPolicy,
};
use tracing::{debug, info, warn};

use crate::cli::{ConnectionArgs, SecurityModeArg, SecurityPolicyArg};
use crate::error::{Result, ScanError};

/// Application identity presented to servers.
const APP_NAME: &str = "opcua-scan";
const APP_URI: &str = "urn:opcua-scan";
const PRODUCT_URI: &str = "urn:opcua-scan";

/// An established, connected OPC UA session plus its lifecycle handles.
pub struct Connection {
    /// The active session. Cheaply cloneable (`Arc`).
    session: Arc<Session>,
    /// Background event loop task handle.
    event_loop: tokio::task::JoinHandle<opcua::types::StatusCode>,
    /// Endpoint that was connected to.
    endpoint: String,
    /// The client, retained for discovery (endpoint listing).
    client: Client,
}

/// Summary of server metadata gathered during `connect`.
#[derive(Debug, Clone)]
pub struct ServerInfo {
    /// Server application URI.
    pub application_uri: String,
    /// Server product URI.
    pub product_uri: String,
    /// Server application name.
    pub application_name: String,
    /// Discovered endpoints.
    pub endpoints: Vec<EndpointSummary>,
}

/// Condensed endpoint description for display.
#[derive(Debug, Clone)]
pub struct EndpointSummary {
    /// Endpoint URL.
    pub url: String,
    /// Security policy URI (shortened to the policy name where possible).
    pub security_policy: String,
    /// Message security mode.
    pub security_mode: String,
    /// Security level (higher is more secure).
    pub security_level: u8,
    /// Supported user token types.
    pub user_tokens: Vec<String>,
}

impl Connection {
    /// Build a client and connect a session using the given connection args.
    pub async fn connect(args: &ConnectionArgs) -> Result<Self> {
        let endpoint = args
            .endpoint
            .clone()
            .ok_or_else(|| ScanError::InvalidArgument("an endpoint URL is required".into()))?;
        validate_endpoint(&endpoint)?;

        let client = build_client(args)?;
        let identity = resolve_identity(args)?;
        let (policy, mode) = (
            security_policy(args.security_policy),
            security_mode(args.security_mode),
        );

        let user_token_policy = match &identity {
            IdentityToken::Anonymous => UserTokenPolicy::anonymous(),
            _ => UserTokenPolicy::anonymous(),
        };

        debug!(endpoint = %endpoint, policy = policy.to_str(), "connecting");
        let endpoint_desc: EndpointDescription =
            (endpoint.as_str(), policy.to_str(), mode, user_token_policy).into();

        let mut client = client;
        let (session, event_loop) = client
            .connect_to_matching_endpoint(endpoint_desc, identity)
            .await
            .map_err(|e| ScanError::Connect {
                endpoint: endpoint.clone(),
                reason: e.to_string(),
            })?;

        let handle = event_loop.spawn();

        // Wait for the connection, bounded by a sensible timeout so we don't hang.
        let connect_timeout = args.timeout.max(Duration::from_secs(5));
        tokio::select! {
            res = session.wait_for_connection() => {
                if !res {
                    return Err(ScanError::Connect {
                        endpoint: endpoint.clone(),
                        reason: "session event loop terminated before connecting".into(),
                    });
                }
            }
            _ = tokio::time::sleep(connect_timeout) => {
                handle.abort();
                return Err(ScanError::ConnectTimeout(connect_timeout));
            }
        }

        info!(endpoint = %endpoint, "connected");
        Ok(Connection {
            session,
            event_loop: handle,
            endpoint,
            client,
        })
    }

    /// Borrow the underlying session.
    pub fn session(&self) -> &Arc<Session> {
        &self.session
    }

    /// The endpoint URL this connection is bound to.
    pub fn endpoint(&self) -> &str {
        &self.endpoint
    }

    /// Gather server metadata by reading endpoints from the server.
    pub async fn server_info(&self) -> Result<ServerInfo> {
        let endpoints = self
            .client
            .get_server_endpoints_from_url(self.endpoint.clone())
            .await
            .map_err(|e| ScanError::service("GetEndpoints", e.status()))?;

        let mut summaries = Vec::with_capacity(endpoints.len());
        let mut app: Option<ApplicationDescription> = None;
        for ep in &endpoints {
            if app.is_none() {
                app = Some(ep.server.clone());
            }
            summaries.push(EndpointSummary {
                url: ep.endpoint_url.to_string(),
                security_policy: shorten_policy_uri(ep.security_policy_uri.as_ref()),
                security_mode: security_mode_name(ep.security_mode),
                security_level: ep.security_level,
                user_tokens: ep
                    .user_identity_tokens
                    .as_ref()
                    .map(|tokens| {
                        tokens
                            .iter()
                            .map(|t| user_token_type_name(t.token_type))
                            .collect()
                    })
                    .unwrap_or_default(),
            });
        }

        let app = app.unwrap_or_default();
        Ok(ServerInfo {
            application_uri: app.application_uri.to_string(),
            product_uri: app.product_uri.to_string(),
            application_name: app.application_name.to_string(),
            endpoints: summaries,
        })
    }

    /// Cleanly disconnect the session and stop the event loop.
    pub async fn disconnect(self) {
        if let Err(e) = self.session.disconnect().await {
            warn!(error = %e, "error during disconnect");
        }
        self.event_loop.abort();
    }
}

/// Build a configured [`Client`] from connection args.
fn build_client(args: &ConnectionArgs) -> Result<Client> {
    let mut builder = ClientBuilder::new()
        .application_name(APP_NAME)
        .application_uri(APP_URI)
        .product_uri(PRODUCT_URI)
        .session_retry_limit(args.retry as i32)
        .session_retry_initial(Duration::from_millis(500))
        .create_sample_keypair(true)
        .trust_server_certs(args.trust_server);

    if let Some(dir) = &args.pki_dir {
        builder = builder.pki_dir(dir.clone());
    }

    if !args.trust_server {
        debug!("server certificate trust is NOT automatic; untrusted certs land in pki/rejected");
    } else {
        warn!("--trust-server enabled: server certificates are trusted automatically (insecure)");
    }

    builder
        .client()
        .map_err(|errs| ScanError::ClientBuild(errs.join("; ")))
}

/// Resolve the identity token from the connection args, never logging secrets.
fn resolve_identity(args: &ConnectionArgs) -> Result<IdentityToken> {
    if let (Some(cert), Some(key)) = (&args.cert, &args.key) {
        return IdentityToken::new_x509_path(cert, key)
            .map_err(|e| ScanError::Credential(format!("failed to load x509 identity: {e}")));
    }

    if let Some(user) = &args.user {
        let password = resolve_password(args)?;
        return Ok(IdentityToken::new_user_name(user.clone(), password));
    }

    Ok(IdentityToken::Anonymous)
}

/// Resolve a password from `--password`, supporting `env:VAR` and secure prompt.
fn resolve_password(args: &ConnectionArgs) -> Result<String> {
    match &args.password {
        Some(raw) => {
            if let Some(var) = raw.strip_prefix("env:") {
                std::env::var(var).map_err(|_| {
                    ScanError::Credential(format!("environment variable '{var}' is not set"))
                })
            } else {
                Ok(raw.clone())
            }
        }
        None => {
            // Prompt securely; the typed characters are never echoed or logged.
            rpassword::prompt_password("Password: ")
                .map_err(|e| ScanError::Credential(format!("failed to read password: {e}")))
        }
    }
}

/// Validate that an endpoint URL is an OPC UA TCP URL.
fn validate_endpoint(endpoint: &str) -> Result<()> {
    if endpoint.starts_with("opc.tcp://") {
        Ok(())
    } else {
        Err(ScanError::InvalidEndpoint(endpoint.to_string()))
    }
}

fn security_policy(arg: SecurityPolicyArg) -> SecurityPolicy {
    match arg {
        SecurityPolicyArg::None => SecurityPolicy::None,
        SecurityPolicyArg::Basic256Sha256 => SecurityPolicy::Basic256Sha256,
        SecurityPolicyArg::Aes128Sha256RsaOaep => SecurityPolicy::Aes128Sha256RsaOaep,
        SecurityPolicyArg::Aes256Sha256RsaPss => SecurityPolicy::Aes256Sha256RsaPss,
    }
}

fn security_mode(arg: SecurityModeArg) -> MessageSecurityMode {
    match arg {
        SecurityModeArg::None => MessageSecurityMode::None,
        SecurityModeArg::Sign => MessageSecurityMode::Sign,
        SecurityModeArg::SignAndEncrypt => MessageSecurityMode::SignAndEncrypt,
    }
}

fn security_mode_name(mode: MessageSecurityMode) -> String {
    match mode {
        MessageSecurityMode::None => "None",
        MessageSecurityMode::Sign => "Sign",
        MessageSecurityMode::SignAndEncrypt => "SignAndEncrypt",
        _ => "Invalid",
    }
    .to_string()
}

fn user_token_type_name(t: opcua::types::UserTokenType) -> String {
    use opcua::types::UserTokenType::*;
    match t {
        Anonymous => "Anonymous",
        UserName => "UserName",
        Certificate => "Certificate",
        IssuedToken => "IssuedToken",
    }
    .to_string()
}

/// Shorten a security policy URI such as
/// `http://opcfoundation.org/UA/SecurityPolicy#Basic256Sha256` to `Basic256Sha256`.
pub fn shorten_policy_uri(uri: &str) -> String {
    uri.rsplit('#').next().unwrap_or(uri).to_string()
}
