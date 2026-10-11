/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */

{ lib, ... }: let
  option = path: [ "services" "gradient" ] ++ path;
  removed = path: lib.mkRemovedOptionModule (option path);
  uploadOptions = "replaced by services.gradient.upload.concurrency and services.gradient.upload.bytesBudget";
in {
  imports = [
    (removed [ "nar" "uploadConcurrency" ] uploadOptions)
    (removed [ "nar" "commitConcurrency" ] uploadOptions)
    (removed [ "nar" "maxBufferBytes" ] uploadOptions)
    (removed [ "scheduler" "recordCandidates" ] "runner-up candidates were never recorded")
    (removed [ "oidc" "iconUrl" ] "the login page never showed the icon")
    (removed [ "gradientCi" "url" ] "the address of Gradient.CI Servers is fixed")
    (removed [ "worker" "build" "cgroupRoot" ] "the Nix daemon reports build resource usage in the build result")
    (removed [ "worker" "drainTimeoutSecs" ] "a stop aborts the running jobs and the server queues them again")
    (lib.mkRenamedOptionModule (option [ "scheduler" "dispatchRetentionDays" ]) (option [ "retentionDays" ]))
    (lib.mkChangedOptionModule (option [ "ssh" "listenAddress" ]) (option [ "ssh" "listenAddresses" ])
      (config: [ config.services.gradient.ssh.listenAddress ]))
  ];
}
