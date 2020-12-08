use hyper::{Body, Method, Response, Server, StatusCode, service::{make_service_fn, service_fn}};
use openidconnect::*;
use openidconnect::core::*;
use openidconnect::PrivateSigningKey;
use openssl::rsa::Rsa;
use serde::{Deserialize, Serialize};
use urlparse::{GetQuery, Query, parse_qs};

use tokio::{sync::oneshot::Sender};

use krill::commons::error::Error;

use std::{collections::HashMap, convert::Infallible, net::SocketAddr, sync::{Arc, Mutex}};
use std::time::Duration;

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CustomAdditionalMetadata {
    end_session_endpoint: String,
}
impl AdditionalProviderMetadata for CustomAdditionalMetadata {}

#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct CustomAdditionalClaims {
    role: String,
}
impl AdditionalClaims for CustomAdditionalClaims {}

// use the CustomAdditionalMetadata type 
type CustomProviderMetadata = ProviderMetadata<
    CustomAdditionalMetadata,
    CoreAuthDisplay,
    CoreClientAuthMethod,
    CoreClaimName,
    CoreClaimType,
    CoreGrantType,
    CoreJweContentEncryptionAlgorithm,
    CoreJweKeyManagementAlgorithm,
    CoreJwsSigningAlgorithm,
    CoreJsonWebKeyType,
    CoreJsonWebKeyUse,
    CoreJsonWebKey,
    CoreResponseMode,
    CoreResponseType,
    CoreSubjectIdentifierType,
>;

// use the CustomAdditionalClaims type, has to be cascaded down a few nesting
// levels of OIDC crate types...
type CustomIdTokenClaims = IdTokenClaims<CustomAdditionalClaims, CoreGenderClaim>;

type CustomIdToken = IdToken<
    CustomAdditionalClaims,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
    CoreJsonWebKeyType,
>;

type CustomIdTokenFields = IdTokenFields<
    CustomAdditionalClaims,
    EmptyExtraTokenFields,
    CoreGenderClaim,
    CoreJweContentEncryptionAlgorithm,
    CoreJwsSigningAlgorithm,
    CoreJsonWebKeyType,
>;

type CustomTokenResponse = StandardTokenResponse<CustomIdTokenFields, CoreTokenType>;
// end cascade

#[derive(Default)]
struct KnownUser {
    role: &'static str,
    _cas: Option<&'static str>,
    token_secs: Option<u32>,
}

struct TempAuthzCodeDetails {
    client_id: String,
    nonce: String,
    username: String,
}
struct LoginSession {
    id: KnownUserId
}

type TempAuthzCode = String;
type TempAuthzCodes = HashMap<TempAuthzCode, TempAuthzCodeDetails>;

type LoggedInAccessToken = String;
type LoginSessions = HashMap<LoggedInAccessToken, LoginSession>;

type KnownUserId = &'static str;
type KnownUsers = HashMap<KnownUserId, KnownUser>;

const DEFAULT_TOKEN_DURATION_SECS: u32 = 3600;

pub async fn start() -> Option<Sender<()>> {
    // let join_handle = task::spawn_blocking(run_mock_openid_connect_server);

    // // wait for the mock OpenID Connect server to be up before continuing
    // // otherwise Krill might fail to query its discovery endpoint
    // while !MOCK_OPENID_CONNECT_SERVER_RUNNING_FLAG.load(Ordering::Relaxed) {
    //     println!("Waiting for mock OpenID Connect server to start");
    //     delay_for(Duration::from_secs(1)).await;
    // }

    // Some(join_handle)
    Some(run_mock_openid_connect_server().await)
}

pub fn stop(tx: Option<Sender<()>>) {
    // MOCK_OPENID_CONNECT_SERVER_RUNNING_FLAG.store(false, Ordering::Relaxed);
    // if let Some(join_handle) = join_handle {
    //     join_handle.await.unwrap();
    // }
    if let Some(tx) = tx {
        tx.send(());
    }
}

async fn run_mock_openid_connect_server() -> Sender<()> {
    // thread::spawn(|| -> tokio::sync::oneshot::Sender<()> {
        let mut authz_codes = TempAuthzCodes::new();
        let mut login_sessions = LoginSessions::new();
        let mut known_users = KnownUsers::new();

        known_users.insert("admin@krill", KnownUser { role: "admin", ..Default::default() });
        known_users.insert("readonly@krill", KnownUser { role: "gui_read_only", ..Default::default() });
        known_users.insert("readwrite@krill", KnownUser { role: "gui_read_write", ..Default::default() });
        known_users.insert("shorttokenwithoutrefresh@krill", KnownUser { role: "gui_read_write", token_secs: Some(1), ..Default::default() });
    
        let provider_metadata: CustomProviderMetadata = ProviderMetadata::new(
            IssuerUrl::new("http://localhost:3001".to_string()).unwrap(),
            AuthUrl::new("http://localhost:3001/authorize".to_string()).unwrap(),
            JsonWebKeySetUrl::new("http://localhost:3001/jwk".to_string()).unwrap(),
            vec![ResponseTypes::new(vec![CoreResponseType::Code])],
            vec![CoreSubjectIdentifierType::Pairwise],
            vec![CoreJwsSigningAlgorithm::RsaSsaPssSha256],
            CustomAdditionalMetadata { end_session_endpoint: String::new() },
        )
        .set_token_endpoint(Some(TokenUrl::new("http://localhost:3001/token".to_string()).unwrap()))
        .set_userinfo_endpoint(
            Some(UserInfoUrl::new("http://localhost:3001/userinfo".to_string()).unwrap())
        )
        .set_scopes_supported(Some(vec![
            Scope::new("openid".to_string()),
            Scope::new("email".to_string()),
            Scope::new("profile".to_string()),
        ]))
        .set_response_modes_supported(Some(vec![CoreResponseMode::Query]))
        .set_id_token_signing_alg_values_supported(vec![CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256])
        .set_claims_supported(Some(vec![CoreClaimName::new("email".to_string())]));
        
        let rsa_key = Rsa::generate(2048).unwrap().private_key_to_pem().unwrap();
        let rsa_pem = std::str::from_utf8(&rsa_key).unwrap();
        let signing_key = CoreRsaPrivateSigningKey::from_pem(
                rsa_pem,
                Some(JsonWebKeyId::new("key1".to_string()))
            ).expect("Invalid RSA private key");

        let jwks = CoreJsonWebKeySet::new(
            vec![
                // RSA keys may also be constructed directly using CoreJsonWebKey::new_rsa(). Providers
                // aiming to support other key types may provide their own implementation of the
                // JsonWebKey trait or submit a PR to add the desired support to this crate.
                signing_key.as_verification_key()
            ]
        );

        let discovery_doc = serde_json::to_string(&provider_metadata)
            .map_err(|err| Error::custom(format!("Error while building discovery JSON response: {}", err))).unwrap();
        let jwks_doc = serde_json::to_string(&jwks)
            .map_err(|err| Error::custom(format!("Error while building jwks JSON response: {}", err))).unwrap();
        let login_doc = std::fs::read_to_string("test-resources/ui/oidc_login.html").unwrap();

        fn make_id_token_response(signing_key: Arc<Mutex<CoreRsaPrivateSigningKey>>, authz: &TempAuthzCodeDetails, session: &LoginSession, known_users: &KnownUsers) -> Result<CustomTokenResponse, Error> {
            let mut access_token_bytes: [u8; 4] = [0; 4];
            openssl::rand::rand_bytes(&mut access_token_bytes)
                .map_err(|err: openssl::error::ErrorStack| Error::custom(format!("Rand error: {}", err)))?;
            let access_token = base64::encode(access_token_bytes);
            let access_token = AccessToken::new(access_token);

            let user = known_users.get(&session.id).ok_or(
                Error::custom(format!("Internal error, unknown user: {}", session.id)))?;

            let token_duration = user.token_secs.unwrap_or(DEFAULT_TOKEN_DURATION_SECS);

            if token_duration != DEFAULT_TOKEN_DURATION_SECS {
                log_warning(&format!("Issuing token with non-default expiration time of {} seconds", &token_duration));
            }

            let signing_key = signing_key.lock().unwrap();
            let id_token = CustomIdToken::new(
                CustomIdTokenClaims::new(
                    // Specify the issuer URL for the OpenID Connect Provider.
                    IssuerUrl::new("http://localhost:3001".to_string()).unwrap(),
                    // The audience is usually a single entry with the client ID of the client for whom
                    // the ID token is intended. This is a required claim.
                    vec![Audience::new(authz.client_id.clone())],
                    // The ID token expiration is usually much shorter than that of the access or refresh
                    // tokens issued to clients.
                    chrono::Utc::now() + chrono::Duration::seconds(token_duration.into()),
                    // The issue time is usually the current time.
                    chrono::Utc::now(),
                    // Set the standard claims defined by the OpenID Connect Core spec.
                    StandardClaims::new(
                        // Stable subject identifiers are recommended in place of e-mail addresses or other
                        // potentially unstable identifiers. This is the only required claim.
                        SubjectIdentifier::new(session.id.to_string())
                    ),
                    CustomAdditionalClaims {
                        role: user.role.to_string()
                    }
                )
                // Optional: specify the user's e-mail address. This should only be provided if the
                // client has been granted the 'profile' or 'email' scopes.
                .set_email(Some(EndUserEmail::new(session.id.to_string())))
                // Optional: specify whether the provider has verified the user's e-mail address.
                .set_email_verified(Some(true))
                // OpenID Connect Providers may supply custom claims by providing a struct that
                // implements the AdditionalClaims trait. This requires manually using the
                // generic IdTokenClaims struct rather than the CoreIdTokenClaims type alias,
                // however.
                .set_nonce(Some(Nonce::new(authz.nonce.clone()))),
                // The private key used for signing the ID token. For confidential clients (those able
                // to maintain a client secret), a CoreHmacKey can also be used, in conjunction
                // with one of the CoreJwsSigningAlgorithm::HmacSha* signing algorithms. When using an
                // HMAC-based signing algorithm, the UTF-8 representation of the client secret should
                // be used as the HMAC key.
                &*signing_key,
                // Uses the RS256 signature algorithm. This crate supports any RS*, PS*, or HS*
                // signature algorithm.
                CoreJwsSigningAlgorithm::RsaSsaPkcs1V15Sha256,
                // When returning the ID token alongside an access token (e.g., in the Authorization Code
                // flow), it is recommended to pass the access token here to set the `at_hash` claim
                // automatically.
                Some(&access_token),
                // When returning the ID token alongside an authorization code (e.g., in the implicit
                // flow), it is recommended to pass the authorization code here to set the `c_hash` claim
                // automatically.
                None,
            ).unwrap();

            // TODO: issue a refresh token?
            // TODO: look at how expiration times are issued and handled, as there are
            // two separate times: access token expiration, and id token expiration.
            let mut token_response = CustomTokenResponse::new(
                access_token,
                CoreTokenType::Bearer,
                CustomIdTokenFields::new(Some(id_token), EmptyExtraTokenFields {}),
            );

            // token_response.set_refresh_token()
            token_response.set_expires_in(Some(&Duration::from_secs(token_duration.into())));
            Ok(token_response)
        }

        fn base64_decode(encoded: String) -> Result<String, Error> {
            String::from_utf8(base64::decode(&encoded)
                .map_err(|err: base64::DecodeError| Error::custom(format!("Base64 decode error: {}", err)))?)
                .map_err(|err: std::string::FromUtf8Error| Error::custom(format!("UTF8 decode error: {}", err)))
        }

        fn url_encode(decoded: String) -> Result<String, Error> {
            urlparse::quote(decoded, b"")
                .map_err(|err: std::string::FromUtf8Error| Error::custom(format!("UTF8 decode error: {}", err)))
        }

        fn require_query_param(query: &Query, param: &str) -> Result<String, Error> {
            query.get_first_from_str(param).ok_or(Error::custom(format!("Missing query parameter '{}'", param)))
        }

        fn str_to_body(in_str: &str) -> Body {
            let res: Result<_, Error> = Ok(in_str.to_string());
            let chunks: Vec<Result<_, _>> = vec![res];
            let stream = futures_util::stream::iter(chunks);
            Body::wrap_stream(stream)
        }

        fn handle_discovery_request(request: hyper::Request<hyper::Body>, discovery_doc: &str) -> Result<hyper::Response<hyper::Body>, Error> {
            Response::builder()
                .header("Content-Type", "application/json")
                .status(StatusCode::OK)
                .body(str_to_body(discovery_doc))
                .map_err(|err| Error::custom(err))
        }

        fn handle_jwks_request(request: hyper::Request<hyper::Body>, jwks_doc: &str) -> Result<hyper::Response<hyper::Body>, Error> {
            Response::builder()
                .header("Content-Type", "application/json")
                .status(StatusCode::OK)
                .body(str_to_body(jwks_doc))
                .map_err(|err| Error::custom(err))
        }

        fn handle_authorize_request(request: hyper::Request<hyper::Body>, login_doc: &str) -> Result<hyper::Response<hyper::Body>, Error> {
            let query = parse_qs(request.uri().query().unwrap_or(""));
            let client_id = require_query_param(&query, "client_id")?;
            let nonce = require_query_param(&query, "nonce")?;
            let state = require_query_param(&query, "state")?;
            let redirect_uri = require_query_param(&query, "redirect_uri")?;
            let body = login_doc
                .replace("<NONCE>", &base64::encode(&nonce))
                .replace("<STATE>", &base64::encode(&state))
                .replace("<REDIRECT_URI>", &base64::encode(&redirect_uri))
                .replace("<CLIENT_ID>", &base64::encode(&client_id));

            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "text/html")
                .body(str_to_body(&body))
                .map_err(|err| Error::custom(err))
        }

        fn handle_login_request(request: hyper::Request<hyper::Body>, authz_codes: &mut TempAuthzCodes, known_users: &KnownUsers) -> Result<hyper::Response<hyper::Body>, Error> {
            let query = parse_qs(request.uri().query().unwrap_or(""));
            let redirect_uri = require_query_param(&query, "redirect_uri")?;
            let redirect_uri = base64_decode(redirect_uri)?;

            fn with_redirect_uri(redirect_uri: String, query: Query, authz_codes: &mut TempAuthzCodes, known_users: &KnownUsers) -> Result<hyper::Response<hyper::Body>, Error> {
                let username = require_query_param(&query, "username")?;

                match known_users.get(username.as_str()) {
                    Some(_user) => {
                        let client_id = require_query_param(&query, "client_id")?;
                        let nonce = require_query_param(&query, "nonce")?;
                        let state = require_query_param(&query, "state")?;

                        let client_id = base64_decode(client_id)?;
                        let nonce = base64_decode(nonce)?;
                        let state = base64_decode(state)?;

                        let mut code_bytes: [u8; 4] = [0; 4];
                        openssl::rand::rand_bytes(&mut code_bytes)
                            .map_err(|err: openssl::error::ErrorStack| Error::custom(format!("Rand error: {}", err)))?;
                        let code = base64::encode(code_bytes);

                        authz_codes.insert(code.clone(), TempAuthzCodeDetails { client_id, nonce: nonce.clone(), username });

                        let urlsafe_code = url_encode(code)?;
                        let urlsafe_state = url_encode(state)?;
                        let urlsafe_nonce = url_encode(nonce)?;

                        Response::builder()
                            .status(StatusCode::FOUND)
                            .header("Location", &format!("{}?code={}&state={}&nonce={}",
                                redirect_uri, urlsafe_code, urlsafe_state, urlsafe_nonce))
                            .body(Body::empty())
                            .map_err(|err| Error::custom(err))
                    },
                    None => Err(Error::custom("Invalid credentials"))
                }
            }

            // per RFC 6749 and OpenID Connect Core 1.0 section 3.1.26
            // Authentication Error Response we should still return a
            // redirect on error but with query params describing the error.
            with_redirect_uri(redirect_uri.clone(), query, authz_codes, known_users)
        }

        fn handle_token_request(mut request: hyper::Request<hyper::Body>, signing_key: Arc<Mutex<CoreRsaPrivateSigningKey>>, authz_codes: &mut TempAuthzCodes, login_sessions: &mut LoginSessions, known_users: &KnownUsers) -> Result<hyper::Response<hyper::Body>, Error> {
            let query_params = parse_qs(request.uri().query().unwrap_or(""));

            if let Some(code) = query_params.get("code") {
                let code = &code[0];
                if let Some(authz_code) = authz_codes.remove(code) {
                    // find static user id
                    let session = LoginSession {
                        id: known_users.keys().find(|k| k.to_string() == authz_code.username)
                            .ok_or(Error::custom(format!("Internal error, unknown user '{}'", authz_code.username)))?
                    };

                    let token_response = make_id_token_response(signing_key, &authz_code, &session, known_users)?;
                    let token_doc = serde_json::to_string(&token_response)
                        .map_err(|err| Error::custom(format!("Error while building ID Token JSON response: {}", err)))?;

                    login_sessions.insert(token_response.access_token().secret().clone(), session);

                    Response::builder()
                        .status(StatusCode::OK)
                        .header("Content-Type", "application/json")
                        .body(str_to_body(&token_doc))
                        .map_err(|err| Error::custom(err))
                } else {
                    Response::builder()
                        .status(StatusCode::BAD_REQUEST)
                        .body(str_to_body(&format!("Unknown temporary authorization code '{}'", &code)))
                        .map_err(|err| Error::custom(err))
                }
            } else {
                Response::builder()
                    .status(StatusCode::BAD_REQUEST)
                    .body(str_to_body("Missing query parameter 'code'"))
                    .map_err(|err| Error::custom(err))
                }
        }

        fn handle_user_info_request(request: hyper::Request<hyper::Body>) -> Result<hyper::Response<hyper::Body>, Error> {
            let standard_claims: StandardClaims<CoreGenderClaim> = StandardClaims::new(SubjectIdentifier::new("sub-123".to_string()));
            let additional_claims = EmptyAdditionalClaims {};
            let claims = UserInfoClaims::new(standard_claims, additional_claims);
            let claims_doc = serde_json::to_string(&claims)
                .map_err(|err| Error::custom(format!("Error while building UserInfo JSON response: {}", err)))?;
            Response::builder()
                .status(StatusCode::OK)
                .header("Content-Type", "application/json")
                .body(str_to_body(&claims_doc))
                .map_err(|err| Error::custom(err))
        }

        async fn handle_request(
            request: hyper::Request<hyper::Body>,
            discovery_doc: &str,
            jwks_doc: &str,
            login_doc: &str,
            signing_key: Arc<Mutex<CoreRsaPrivateSigningKey>>,
            authz_codes: &mut TempAuthzCodes,
            login_sessions: &mut LoginSessions,
            known_users: &KnownUsers)
        -> Result<hyper::Response<hyper::Body>, Error> {
            match *request.method() {
                Method::GET => {
                    match request.uri().path() {
                        "/.well-known/openid-configuration" => {
                            handle_discovery_request(request, discovery_doc)
                        },
                        "/jwk" => {
                            handle_jwks_request(request, jwks_doc)
                        },
                        "/authorize" => {
                            handle_authorize_request(request, login_doc)
                        },
                        "/login_form_submit" => {
                            handle_login_request(request, authz_codes, known_users)
                        },
                        "/userinfo" => {
                            handle_user_info_request(request)
                        }
                        _ => {
                            Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Body::empty())
                                .map_err(|err| Error::custom(err))
                        }
                    }
                },
                Method::POST => {
                    match request.uri().path() {
                        "/token" => {
                            handle_token_request(request, signing_key, authz_codes, login_sessions, known_users)
                        },
                        _ => {
                            Response::builder()
                                .status(StatusCode::NOT_FOUND)
                                .body(Body::empty())
                                .map_err(|err| Error::custom(err))
                        }
                    }
                },
                _ => {
                    Response::builder()
                        .status(StatusCode::METHOD_NOT_ALLOWED)
                        .body(Body::empty())
                        .map_err(|err| Error::custom(err))
                }
            }
        }

        fn log_error(err: Error) {
            eprintln!("Mock OpenID Connect server: ERROR: {}", err);
        }

        fn log_warning(warning: &str) {
            eprintln!("Mock OpenID Connect server: WARNING: {}", warning);
        }

        let address = "127.0.0.1:3001";
        println!("Mock OpenID Connect server: starting on {}", address);

        // let server = Server::http(address).unwrap();
        // MOCK_OPENID_CONNECT_SERVER_RUNNING_FLAG.store(true, Ordering::Relaxed);
        // while MOCK_OPENID_CONNECT_SERVER_RUNNING_FLAG.load(Ordering::Relaxed) {
        //     match server.recv_timeout(Duration::new(1, 0)) {
        //         Ok(None) => { /* no request received within the timeout */ },
        //         Ok(Some(request)) => {
        //             if let Err(err) = handle_request(request, &discovery_doc, &jwks_doc, &login_doc, &signing_key, &mut authz_codes, &mut login_sessions, &known_users) {
        //                 log_error(err);
        //             }
        //         },
        //         Err(err) => { 
        //             log_error(err.into());
        //         }
        //     };
        // }

        let addr: SocketAddr = address.parse().unwrap();

        let (tx, rx) = tokio::sync::oneshot::channel::<()>();

        let signing_key = Arc::new(Mutex::new(signing_key));

        let service = make_service_fn(move |_| {
            let signing_key_capture = signing_key.clone();
            async {
                Ok::<_, Infallible>(service_fn(move |req: hyper::Request<hyper::Body>| {
                    handle_request(req, &discovery_doc, &jwks_doc, &login_doc, signing_key.clone(), &mut authz_codes, &mut login_sessions, &known_users)
                }))
            }
        });

        let server = Server::bind(&addr).serve(service);

        let graceful = server.with_graceful_shutdown(async {
            rx.await.ok();
            println!("Mock OpenID Connect: stopping");
        });

        if let Err(err) = graceful.await {
            log_error(Error::custom(format!("Server error: {}", err)));
        }

        tx
    // });
}