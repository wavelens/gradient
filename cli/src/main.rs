/*
 * SPDX-FileCopyrightText: 2024 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;
mod request;

use clap::{arg, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use config::*;
use rpassword::read_password;
use std::io;
use std::io::Write;
use std::process::exit;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<MainCommands>,
    #[arg(long, value_enum)]
    generate_completions: Option<Shell>,
}

#[derive(Subcommand, Debug)]
enum MainCommands {
    Config {
        key: String,
        value: Option<String>,
    },
    Status,
    Register {
        #[arg(short, long)]
        username: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        email: Option<String>,
    },
    Login {
        #[arg(short, long)]
        username: Option<String>,
    },
    Logout,
    Organization {
        organization_id: Option<String>,
        #[command(subcommand)]
        cmd: OrganizationCommands,
    },
    Project {
        #[command(subcommand)]
        cmd: ProjectCommands,
    },
    Server {
        #[command(subcommand)]
        cmd: ServerCommands,
    },
}

#[derive(Subcommand, Debug)]
enum OrganizationCommands {
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short = 's', long, default_value = "true")]
        use_nix_store: bool,
    },
    List,
    Delete,
    Ssh {
        #[command(subcommand)]
        cmd: SshCommands,
    },
}

#[derive(Subcommand, Debug)]
enum SshCommands {
    Show,
    Recreate,
}

#[derive(Subcommand, Debug)]
enum ProjectCommands {
    Create {
        #[arg(short, long)]
        organization_id: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        description: Option<String>,
        #[arg(short, long)]
        repository: Option<String>,
        #[arg(short = 'w', long)]
        evaluation_wildcard: Option<String>,
    },
    List,
    Delete,
}

#[derive(Subcommand, Debug)]
enum ServerCommands {
    Create {
        #[arg(short, long)]
        organization_id: Option<String>,
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short = 's', long)]
        host: Option<String>,
        #[arg(short, long)]
        port: Option<i32>,
        #[arg(short = 'u', long)]
        ssh_user: Option<String>,
        #[arg(short, long)]
        architectures: Option<String>,
        #[arg(short, long)]
        features: Option<String>,
    },
    List,
    Delete {
        id: Option<String>,
    },
}

fn ask_for_input(prompt: &str) -> String {
    print!("{}: ", prompt);
    std::io::stdout().flush().unwrap();
    let mut inp = String::new();
    io::stdin()
        .read_line(&mut inp)
        .expect(format!("Failed to read {}.", prompt).as_str());
    let inp = inp.trim().to_string();

    if inp.is_empty() {
        eprintln!("{} cannot be empty.", prompt);
        exit(1);
    }

    inp
}

fn ask_for_password() -> String {
    print!("Password: ");
    std::io::stdout().flush().unwrap();
    let inp = read_password().unwrap();

    if inp.is_empty() {
        eprintln!("Password cannot be empty.");
        exit(1);
    }

    inp
}

fn ask_for_list(prompt: &str) -> Vec<String> {
    print!("{} (list of items separated by commas): ", prompt);
    std::io::stdout().flush().unwrap();
    let mut inp = String::new();
    io::stdin()
        .read_line(&mut inp)
        .expect("Failed to read list.");
    inp.trim()
        .split(",")
        .map(|s| s.trim().to_string())
        .collect()
}

// fn very_input(prompt: &str, input: &str) -> bool {
//     loop {
//         print!("Do you really want to {}: {} [y/n]", prompt, input);
//         std::io::stdout().flush().unwrap();
//         let mut inp = String::new();
//         io::stdin().read_line(&mut inp).expect(format!("Failed to read {}.", prompt).as_str());
//         return inp.trim().to_lowercase() == "y";
//     };
// }

#[tokio::main]
pub async fn main() -> std::io::Result<()> {
    let cli = Cli::parse();

    if let Some(shell) = cli.generate_completions {
        let mut app = Cli::command();
        let bin_name = app.get_name().to_string();
        generate(shell, &mut app, bin_name, &mut io::stdout());
        return Ok(());
    }

    if let Some(cmd) = cli.cmd {
        match cmd {
            MainCommands::Config { key, value } => {
                set_get_value_from_string(key, value, false)
                    .map_err(|_| {
                        exit(1);
                    })
                    .unwrap();
            }

            MainCommands::Status => {
                let config = load_config();
                let server_url = set_get_value(ConfigKey::Server, None, true);
                let auth_token = set_get_value(ConfigKey::AuthToken, None, true);

                if server_url.is_none() {
                    eprintln!(
                        "Server URL is not set. Use `gradient config server <url>` to set it."
                    );
                    std::process::exit(1);
                }

                if auth_token.is_none() {
                    eprintln!("Not logged in. Use `gradient login` to log in.");
                    std::process::exit(1);
                }

                request::health(config)
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                println!("Server Online.");
            }

            MainCommands::Register {
                username,
                name,
                email,
            } => {
                let server_url = set_get_value(ConfigKey::Server, None, true);

                if server_url.is_none() {};

                let username = match username {
                    Some(username) => username,
                    None => ask_for_input("Username"),
                };

                let name = match name {
                    Some(name) => name,
                    None => ask_for_input("Name"),
                };

                let email = match email {
                    Some(email) => email,
                    None => ask_for_input("Email"),
                };

                let password = ask_for_password();

                let res = request::register(load_config(), username, name, email, password)
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                if res.error {
                    eprintln!("Registration failed: {}", res.message);
                    exit(1);
                } else {
                    println!("Registration successful. Please log in.");
                }
            }

            MainCommands::Login { username } => {
                let server_url = set_get_value(ConfigKey::Server, None, true);

                if server_url.is_none() {
                    set_get_value(ConfigKey::Server, Some(ask_for_input("Server URL: ")), true)
                        .unwrap();
                };

                let username = if let Some(username) = username {
                    username
                } else {
                    ask_for_input("Username")
                };

                let password = ask_for_password();

                let res = request::login(load_config(), username, password)
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                if res.error {
                    eprintln!("Login failed: {}", res.message);
                    exit(1);
                } else {
                    set_get_value(ConfigKey::AuthToken, Some(res.message), true).unwrap();
                }
            }

            MainCommands::Logout => {
                set_get_value(ConfigKey::AuthToken, Some(String::new()), true).unwrap();
                println!("Logged out.");
            }

            MainCommands::Organization {
                cmd,
                organization_id,
            } => match cmd {
                OrganizationCommands::Create {
                    name,
                    description,
                    use_nix_store,
                } => {
                    let name = match name {
                        Some(name) => name,
                        None => ask_for_input("Name"),
                    };

                    let description = match description {
                        Some(description) => description,
                        None => ask_for_input("Description"),
                    };

                    let res = request::create_organization(
                        load_config(),
                        name,
                        description,
                        use_nix_store,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Organization creation failed: {}", res.message);
                        exit(1);
                    } else {
                        println!("Organization created.");
                    }
                }

                OrganizationCommands::List => {
                    let res = request::list_organization(load_config())
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                    if res.error {
                        eprintln!("Failed to list organizations");
                        exit(1);
                    } else if res.message.is_empty() {
                        println!("You have no organizations.");
                    } else {
                        for org in res.message {
                            println!("{}: {}", org.name, org.id);
                        }
                    }
                }

                OrganizationCommands::Delete => {
                    let organization_id = match organization_id {
                        Some(id) => id,
                        None => {
                            eprintln!("Organization ID is required for command.");
                            exit(1);
                        }
                    };

                    let res = request::delete_organization(load_config(), organization_id)
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                    if res.error {
                        eprintln!("Failed to delete organization: {}", res.message);
                        exit(1);
                    } else {
                        println!("Organization deleted.");
                    }
                }

                OrganizationCommands::Ssh { cmd } => {
                    let organization_id = match organization_id {
                        Some(id) => id,
                        None => {
                            eprintln!("Organization ID is required for command.");
                            exit(1);
                        }
                    };

                    match cmd {
                        SshCommands::Show => {
                            let res = request::get_organization_ssh(load_config(), organization_id)
                                .await
                                .map_err(|e| {
                                    eprintln!("{}", e);
                                    exit(1);
                                })
                                .unwrap();

                            if res.error {
                                eprintln!("Failed to show SSH key: {}", res.message);
                                exit(1);
                            } else {
                                println!("Public Key: {}", res.message);
                            }
                        }

                        SshCommands::Recreate => {
                            let res =
                                request::renew_organization_ssh(load_config(), organization_id)
                                    .await
                                    .map_err(|e| {
                                        eprintln!("{}", e);
                                        exit(1);
                                    })
                                    .unwrap();

                            if res.error {
                                eprintln!("Failed to recreate SSH key: {}", res.message);
                            } else {
                                println!("New Public Key: {}", res.message);
                            }
                        }
                    }
                }
            },

            MainCommands::Project { cmd } => {
                match cmd {
                    ProjectCommands::Create {
                        organization_id,
                        name,
                        description,
                        repository,
                        evaluation_wildcard,
                    } => {
                        let organization_id = match organization_id {
                            Some(id) => id,
                            None => ask_for_input("Organization ID"),
                        };

                        let name = match name {
                            Some(name) => name,
                            None => ask_for_input("Name"),
                        };

                        let description = match description {
                            Some(description) => description,
                            None => ask_for_input("Description"),
                        };

                        let repository = match repository {
                            Some(repository) => repository,
                            None => ask_for_input("Repository"),
                        };

                        let evaluation_wildcard = match evaluation_wildcard {
                            Some(evaluation_wildcard) => evaluation_wildcard,
                            None => ask_for_input("Evaluation Wildcard"),
                        };

                        let res = request::create_project(
                            load_config(),
                            organization_id,
                            name,
                            description,
                            repository,
                            evaluation_wildcard,
                        )
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                        if res.error {
                            eprintln!("Project creation failed: {}", res.message);
                            exit(1);
                        } else {
                            println!("Project created.");
                        }
                    }

                    ProjectCommands::List => {
                        // let res = request::list_project(load_config()).await.map_err(|e| {
                        //     eprintln!("{}", e);
                        //     exit(1);
                        // }).unwrap();

                        // if res.error {
                        //     eprintln!("Failed to list projects");
                        //     exit(1);
                        // } else if res.message.is_empty() {
                        //    println!("You have no projects.");
                        // } else {
                        //     for proj in res.message {
                        //         println!("{}", proj.name);
                        //     }
                        // }
                        eprintln!("Not implemented yet.");
                        exit(1);
                    }

                    ProjectCommands::Delete => {
                        eprintln!("Not implemented yet.");
                        exit(1);
                    }
                }
            }

            MainCommands::Server { cmd } => {
                match cmd {
                    ServerCommands::Create {
                        organization_id,
                        name,
                        host,
                        port,
                        ssh_user,
                        architectures,
                        features,
                    } => {
                        let organization_id = match organization_id {
                            Some(id) => id,
                            None => ask_for_input("Organization ID"),
                        };

                        let name = match name {
                            Some(name) => name,
                            None => ask_for_input("Name"),
                        };

                        let host = match host {
                            Some(host) => host,
                            None => ask_for_input("Host"),
                        };

                        let port = match port {
                            Some(port) => port,
                            None => ask_for_input("Port")
                                .parse::<i32>()
                                .map_err(|_| {
                                    eprintln!("Not a valid port.");
                                    exit(1);
                                })
                                .unwrap(),
                        };

                        let ssh_user = match ssh_user {
                            Some(ssh_user) => ssh_user,
                            None => ask_for_input("SSH User"),
                        };

                        let architectures = match architectures {
                            Some(architectures) => architectures
                                .split(",")
                                .map(|s| s.trim().to_string())
                                .collect(),
                            None => ask_for_list("Architectures"),
                        };

                        let features = match features {
                            Some(features) => {
                                features.split(",").map(|s| s.trim().to_string()).collect()
                            }
                            None => ask_for_list("Features"),
                        };

                        let res = request::create_server(
                            load_config(),
                            organization_id,
                            name,
                            host,
                            port,
                            ssh_user,
                            architectures,
                            features,
                        )
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                        if res.error {
                            eprintln!("Server creation failed: {}", res.message);
                            exit(1);
                        } else {
                            println!("Server created.");
                        }
                    }

                    ServerCommands::List => {
                        let res = request::list_server(load_config())
                            .await
                            .map_err(|e| {
                                eprintln!("{}", e);
                                exit(1);
                            })
                            .unwrap();

                        if res.error {
                            eprintln!("Failed to list servers");
                            exit(1);
                        } else if res.message.is_empty() {
                            println!("You have no servers.");
                        } else {
                            for server in res.message {
                                println!("{}: {}", server.name, server.id);
                            }
                        }
                    }

                    ServerCommands::Delete { id } => {
                        // let id = match id {
                        //     Some(id) => id,
                        //     None => ask_for_input("Server ID")
                        // };

                        // let res = request::delete_server(load_config(), id).await.map_err(|e| {
                        //     eprintln!("{}", e);
                        //     exit(1);
                        // }).unwrap();

                        // if res.error {
                        //     eprintln!("Failed to delete server: {}", res.message);
                        //     exit(1);
                        // } else {
                        //     println!("Server deleted.");
                        // }
                    }
                }
            }
        }
    }

    exit(1);
}
