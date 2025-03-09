/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

mod config;

use clap::{arg, CommandFactory, Parser, Subcommand};
use clap_complete::{generate, Shell};
use config::*;
use connector::*;
use rpassword::read_password;
use std::collections::HashMap;
use std::{io, fs};
use std::io::Write;
use std::process::exit;
use std::process::Command;

#[derive(Parser, Debug)]
#[command(name = "Gradient", display_name = "Gradient", bin_name = "gradient", author = "Wavelens", version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    cmd: Option<MainCommands>,
    #[arg(long, value_enum)]
    generate_completions: Option<Shell>,
}

// TODO: display help when no subcommand is given
// TODO: check selected organization and project before running commands
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
    Info,
    Organization {
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
    Select {
        organization: String,
    },
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
        description: Option<String>,
    },
    Show,
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
    Select {
        project: String,
    },
    Show,
    Log,
    Create {
        #[arg(short, long)]
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
        #[arg(short = 'c', long)]
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
        name: Option<String>,
        #[arg(short, long)]
        display_name: Option<String>,
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

// fn very_input(prompt: &str, input: &str) -> bool {
//     loop {
//         print!("Do you really want to {}: {} [y/n]", prompt, input);
//         std::io::stdout().flush().unwrap();
//         let mut inp = String::new();
//         io::stdin().read_line(&mut inp).expect(format!("Failed to read {}.", prompt).as_str());
//         return inp.trim().to_lowercase() == "y";
//     };
// }

fn handle_input(values: Vec<(String, Option<String>)>) -> HashMap<String, String> {
    if values.is_empty() {
        println!("No input fields");
        exit(1);
    }

    if !values.iter().any(|(_, v)| v.is_none()) {
        return values.iter().map(|(k, v)| {
            (k.clone(), v.clone().unwrap())
        }).collect();
    }

    let input_fields: String = values.iter().map(|(k, v)| {
        format!(
            "{}: {}\n",
            k,
            if let Some(val) = v {
                val.clone()
            } else {
                "".to_string()
            }
        )
    }).collect();

    let name = "./GRADIENT-CONFIGURATOR";

    if fs::exists(name).unwrap() {
        println!("GRADIENT-CONFIGURATOR file currently open");
        exit(1);
    }

    let mut file = fs::File::create(name).unwrap();
    file.write_all(input_fields.as_bytes()).unwrap();

    let editor = std::env::var("EDITOR").unwrap();
    let output = Command::new(editor.clone())
        .arg(name)
        .status()
        .unwrap();

    if !output.success() {
        println!("Failed to open editor {}", editor);
        exit(1);
    }

    let contents = fs::read_to_string(name).unwrap();
    fs::remove_file(name).unwrap();

    let mut result: HashMap<String, String> = HashMap::new();
    for line in contents.lines() {
        let parts: Vec<&str> = line.split(":").map(|v| v.trim()).collect();
        if parts.len() != 2 {
            eprintln!("Invalid input: {}", line);
            exit(1);
        }

        if !values.iter().any(|(k, _)| k == parts[0]) {
            eprintln!("Invalid input field: {}", parts[0]);
            exit(1);
        }

        if parts[1].is_empty() {
            eprintln!("{} cannot be empty.", parts[0]);
            exit(1);
        }

        result.insert(parts[0].to_string(), parts[1].to_string());
    }

    result
}

fn get_request_config(config: HashMap<ConfigKey, Option<String>>) -> Result<RequestConfig, String> {
    let server_url: String =
        if let Some(server_url) = config.get(&ConfigKey::Server).unwrap().clone() {
            server_url
        } else {
            return Err(
                "Server URL not set. Use `gradient config server <url>` to set it.".to_string(),
            );
        };

    let token = set_get_value(ConfigKey::AuthToken, None, true);

    Ok(RequestConfig { server_url, token })
}

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

                health(get_request_config(config).unwrap())
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

                let input_fields = vec![
                    ("Username", username),
                    ("Name", name),
                    ("Email", email),
                ].iter().map(|(k, v)| {
                    (k.to_string(), v.clone())
                }).collect();

                let input = handle_input(input_fields);

                let password = ask_for_password();

                let res = auth::post_basic_register(
                    get_request_config(load_config()).unwrap(),
                    input.get("Username").unwrap().clone(),
                    input.get("Name").unwrap().clone(),
                    input.get("Email").unwrap().clone(),
                    password,
                )
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

                if res.error {
                    eprintln!("Registration failed: {}", res.message);
                    exit(1);
                }

                println!("Registration successful. Please log in.");
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

                let res = auth::post_basic_login(
                    get_request_config(load_config()).unwrap(),
                    username,
                    password,
                )
                .await
                .map_err(|e| {
                    eprintln!("{}", e);
                    exit(1);
                })
                .unwrap();

                if res.error {
                    eprintln!("Login failed: {}", res.message);
                    exit(1);
                }

                set_get_value(ConfigKey::AuthToken, Some(res.message), true).unwrap();
            }

            MainCommands::Info => {
                let res = user::get(get_request_config(load_config()).unwrap())
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                if res.error {
                    eprintln!("Failed to get user info.");
                    exit(1);
                }

                println!("User ID: {}", res.message.id);
                println!("Username: {}", res.message.username);
                println!("Name: {}", res.message.name);
                println!("Email: {}", res.message.email);
            }

            MainCommands::Logout => {
                set_get_value(ConfigKey::AuthToken, Some(String::new()), true).unwrap();
                println!("Logged out.");
            }

            MainCommands::Organization { cmd } => match cmd {
                OrganizationCommands::Select { organization } => {
                    set_get_value(ConfigKey::SelectedOrganization, Some(organization), true)
                        .unwrap();
                    println!("Organization selected.");
                }

                OrganizationCommands::Create {
                    name,
                    display_name,
                    description,
                } => {
                    let input_fields = vec![
                        ("Name", name),
                        ("Display Name", display_name),
                        ("Description", description),
                    ].iter().map(|(k, v)| {
                        (k.to_string(), v.clone())
                    }).collect();

                    let input = handle_input(input_fields);
                    let name = input.get("Name").unwrap().clone();

                    let res = orgs::put(
                        get_request_config(load_config()).unwrap(),
                        name.clone(),
                        input.get("Display Name").unwrap().clone(),
                        input.get("Description").unwrap().clone(),
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
                    }

                    set_get_value(ConfigKey::SelectedOrganization, Some(name), true);
                    println!("Organization created.");
                }

                OrganizationCommands::Show => {
                    let organization =
                        match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                            Some(id) => id,
                            None => {
                                eprintln!("Organization is required for command.");
                                exit(1);
                            }
                        };

                    let res = orgs::get_organization(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to show organization.");
                        exit(1);
                    }

                    println!("Name: {}", res.message.name);
                    println!("Description: {}", res.message.description);
                    println!("Use Nix Store: {}", res.message.use_nix_store);
                }

                OrganizationCommands::List => {
                    let res = orgs::get(get_request_config(load_config()).unwrap())
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                    if res.error {
                        eprintln!("Failed to list organizations");
                        exit(1);
                    }

                    if res.message.is_empty() {
                        println!("You have no organizations.");
                    } else {
                        for org in res.message {
                            println!("{}: {}", org.name, org.id);
                        }
                    }
                }

                OrganizationCommands::Delete => {
                    let organization =
                        match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                            Some(id) => id,
                            None => {
                                eprintln!("Organization is required for command.");
                                exit(1);
                            }
                        };

                    let res = orgs::delete_organization(
                        get_request_config(load_config()).unwrap(),
                        organization,
                    )
                    .await
                    .map_err(|e| {
                        eprintln!("{}", e);
                        exit(1);
                    })
                    .unwrap();

                    if res.error {
                        eprintln!("Failed to delete organization: {}", res.message);
                        exit(1);
                    }

                    println!("Organization deleted.");
                }

                OrganizationCommands::Ssh { cmd } => {
                    let organization =
                        match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                            Some(id) => id,
                            None => {
                                eprintln!("Organization is required for command.");
                                exit(1);
                            }
                        };

                    match cmd {
                        SshCommands::Show => {
                            let res = orgs::get_organization_ssh(
                                get_request_config(load_config()).unwrap(),
                                organization,
                            )
                            .await
                            .map_err(|e| {
                                eprintln!("{}", e);
                                exit(1);
                            })
                            .unwrap();

                            if res.error {
                                eprintln!("Failed to show SSH key: {}", res.message);
                                exit(1);
                            }

                            println!("Public Key: {}", res.message);
                        }

                        SshCommands::Recreate => {
                            let res = orgs::post_organization_ssh(
                                get_request_config(load_config()).unwrap(),
                                organization,
                            )
                            .await
                            .map_err(|e| {
                                eprintln!("{}", e);
                                exit(1);
                            })
                            .unwrap();

                            if res.error {
                                eprintln!("Failed to recreate SSH key: {}", res.message);
                                exit(1);
                            }

                            println!("New Public Key: {}", res.message);
                        }
                    }
                }
            },

            MainCommands::Project { cmd } => {
                match cmd {
                    ProjectCommands::Select { project } => {
                        let organization =
                            match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                                Some(id) => id,
                                None => {
                                    eprintln!("Organization is required for command.");
                                    exit(1);
                                }
                            };

                        set_get_value(
                            ConfigKey::SelectedProject,
                            Some(format!("{}/{}", organization, project)),
                            true,
                        )
                        .unwrap();
                        println!("Project selected in current Organization.");
                    }

                    ProjectCommands::Show => {
                        let (organization, project) =
                            match set_get_value(ConfigKey::SelectedProject, None, true) {
                                Some(id) => {
                                    let parts: Vec<&str> = id.split("/").collect();
                                    (parts[0].to_string(), parts[1].to_string())
                                }
                                None => {
                                    eprintln!("Project is required for command.");
                                    exit(1);
                                }
                            };

                        let project = projects::get_project(
                            get_request_config(load_config()).unwrap(),
                            organization,
                            project,
                        )
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                        if project.error {
                            eprintln!("Failed to show project.");
                            exit(1);
                        }

                        println!("===== Project =====");
                        println!("Name: {}", project.message.name);
                        println!("Description: {}", project.message.description);
                        println!("Repository: {}", project.message.repository);
                        println!(
                            "Evaluation Wildcard: {}",
                            project.message.evaluation_wildcard
                        );
                        println!("Organization ID: {}", project.message.organization);
                        println!("Last Check At: {}", project.message.last_check_at);
                        println!();

                        if project.message.last_evaluation.is_none() {
                            println!("No last evaluation.");
                            exit(0);
                        }

                        let evaluation = evals::get_evaluation(
                            get_request_config(load_config()).unwrap(),
                            project.message.last_evaluation.unwrap(),
                        )
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                        if evaluation.error {
                            eprintln!("Failed to show evaluation.");
                            exit(1);
                        }

                        println!("===== Evaluation =====");
                        println!("ID: {}", evaluation.message.id);
                        println!("Status: {}", evaluation.message.status);
                        println!("Commit: {}", evaluation.message.commit);
                        println!();

                        let builds = evals::get_evaluation_builds(
                            get_request_config(load_config()).unwrap(),
                            evaluation.message.id.clone(),
                        )
                        .await
                        .map_err(|e| {
                            eprintln!("{}", e);
                            exit(1);
                        })
                        .unwrap();

                        if builds.error {
                            eprintln!("Failed to get builds.");
                            exit(1);
                        }

                        if builds.message.is_empty() {
                            println!("No builds.");
                            exit(0);
                        }

                        println!("===== Building =====");
                        for build in builds.message.clone() {
                            println!("{}", build.name);
                        }
                        println!();

                        println!("===== Log =====");
                        evals::connect_evaluation(
                            get_request_config(load_config()).unwrap(),
                            evaluation.message.id,
                        )
                        .await
                        .unwrap();
                    }

                    ProjectCommands::Log => {
                        todo!();
                    }

                    ProjectCommands::Create {
                        name,
                        display_name,
                        description,
                        repository,
                        evaluation_wildcard,
                    } => {
                        let organization =
                            match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                                Some(id) => id,
                                None => {
                                    eprintln!("Organization is required for command.");
                                    exit(1);
                                }
                            };

                        let input_fields = vec![
                            ("Name", name),
                            ("Display Name", display_name),
                            ("Description", description),
                            ("Repository", repository),
                            ("Evaluation Wildcard", evaluation_wildcard),
                        ].iter().map(|(k, v)| {
                            (k.to_string(), v.clone())
                        }).collect();

                        let input = handle_input(input_fields);
                        let name = input.get("Name").unwrap().clone();

                        let res = projects::put(
                            get_request_config(load_config()).unwrap(),
                            organization.clone(),
                            name.clone(),
                            input.get("Display Name").unwrap().clone(),
                            input.get("Description").unwrap().clone(),
                            input.get("Repository").unwrap().clone(),
                            input.get("Evaluation Wildcard").unwrap().clone(),
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
                        }

                        set_get_value(
                            ConfigKey::SelectedProject,
                            Some(format!("{}/{}", organization, name)),
                            true,
                        )
                        .unwrap();
                        println!("Project created.");
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
                        name,
                        display_name,
                        host,
                        port,
                        ssh_user,
                        architectures,
                        features,
                    } => {
                        let organization =
                            match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                                Some(id) => id,
                                None => {
                                    eprintln!("Organization is required for command.");
                                    exit(1);
                                }
                            };

                        let input_fields = vec![
                            ("Name", name),
                            ("Display Name", display_name),
                            ("Host", host),
                            ("Port", port.map(|p| p.to_string())),
                            ("SSH User", ssh_user),
                            ("Architectures", architectures),
                            ("Features", features),
                        ].iter().map(|(k, v)| {
                            (k.to_string(), v.clone())
                        }).collect();

                        let input = handle_input(input_fields);
                        let name = input.get("Name").unwrap().clone();

                        let res = servers::put(
                            get_request_config(load_config()).unwrap(),
                            organization,
                            name,
                            input.get("Display Name").unwrap().clone(),
                            input.get("Host").unwrap().clone(),
                            input.get("Port").unwrap().parse().unwrap(),
                            input.get("SSH User").unwrap().clone(),
                            input.get("Architectures").unwrap().split(",").map(|s| s.to_string()).collect(),
                            input.get("Features").unwrap().split(",").map(|s| s.to_string()).collect(),
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
                        }

                        println!("Server created.");
                    }

                    ServerCommands::List => {
                        let organization =
                            match set_get_value(ConfigKey::SelectedOrganization, None, true) {
                                Some(id) => id,
                                None => {
                                    eprintln!("Organization is required for command.");
                                    exit(1);
                                }
                            };

                        let res =
                            servers::get(get_request_config(load_config()).unwrap(), organization)
                                .await
                                .map_err(|e| {
                                    eprintln!("{}", e);
                                    exit(1);
                                })
                                .unwrap();

                        if res.error {
                            eprintln!("Failed to list servers");
                            exit(1);
                        }

                        if res.message.is_empty() {
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
                        //     None => ask_for_input("Server")
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
