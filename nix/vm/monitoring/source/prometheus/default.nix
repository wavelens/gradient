/*
 * SPDX-FileCopyrightText: 2025 Wavelens UG <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ config, ... }:
{
  services.prometheus = {
    enable = true;
    retentionTime = "3y";

    globalConfig.scrape_interval = "1s"; #1m
    exporters = {
      postgres = {
        enable = true;
        runAsLocalSuperUser = true;
        extraFlags =  [ "--no-collector.stat_bgwriter" ]; # broken as of, 10.11.24. See: `https://github.com/prometheus-community/postgres_exporter/issues/1060`.
      };
      node = {
        enable = true;
        enabledCollectors = [ "systemd" "ethtool" ];
      };
    };
    scrapeConfigs = [
      {
        job_name = "postgres";
          static_configs = [{
            targets = [ "localhost:${toString config.services.prometheus.exporters.postgres.port}" ];
          }];
      }
      {
        job_name = "node";
          static_configs = [{
            targets = [ "localhost:${toString config.services.prometheus.exporters.node.port}" ];
        }];
      }
    ];
  };
}
