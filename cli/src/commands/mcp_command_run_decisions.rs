/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

/// Whether MCP tool `run_gradient_command` can run a command. A group makes `CommandNotAllowed` apply to all commands under it. `McpToolMayRunCommand` is valid only on leaf commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum McpToolCommandPermission {
    McpToolMayRunCommand,
    CommandNotAllowed(&'static str),
}

impl clap::builder::CommandExt for McpToolCommandPermission {}

pub const NOT_AVAILABLE_BECAUSE_COMMAND_USES_LOCAL_FILES: McpToolCommandPermission =
    McpToolCommandPermission::CommandNotAllowed("This command can write to or read local files.");

pub const UNAVAILABLE_BECAUSE_COMMAND_REVEALS_OR_SETS_SECRET_VALUES: McpToolCommandPermission =
    McpToolCommandPermission::CommandNotAllowed("This command can display or set secret values.");

pub const REQUIRES_INTERACTIVE_TERMINAL: McpToolCommandPermission =
    McpToolCommandPermission::CommandNotAllowed(
        "This command can only run in an interactive terminal.",
    );

pub const COMMAND_NOT_OFFERED_TO_MODEL: McpToolCommandPermission =
    McpToolCommandPermission::CommandNotAllowed(
        "The MCP tool doesn't provide this command group to the model.",
    );
