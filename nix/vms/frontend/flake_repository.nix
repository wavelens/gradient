/*
 * SPDX-FileCopyrightText: 2026 Wavelens GmbH <info@wavelens.io>
 *
 * SPDX-License-Identifier: AGPL-3.0-only
 */
{
  description = "Test Repository for Gradient Build Server";
  outputs = { self, ... }: {
    packages.x86_64-linux = let
      # Base derivation - no dependencies
      base = builtins.derivation {
        name = "base-package";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo 'Base package content' > $out" ];
      };

      # Layer 1 - depends on base
      lib1 = builtins.derivation {
        name = "lib1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${base.out} > $out && echo 'lib1 addition' >> $out" ];
      };

      lib2 = builtins.derivation {
        name = "lib2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${base.out} > $out && echo 'lib2 addition' >> $out" ];
      };

      # Layer 2 - depends on layer 1
      util1 = builtins.derivation {
        name = "util1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${lib1.out} ${lib2.out} > $out && echo 'util1 combination' >> $out" ];
      };

      util2 = builtins.derivation {
        name = "util2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${lib1.out} > $out && echo 'util2 processing' >> $out" ];
      };

      util3 = builtins.derivation {
        name = "util3";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${lib2.out} > $out && echo 'util3 processing' >> $out" ];
      };

      # Layer 3 - depends on layer 2
      tool1 = builtins.derivation {
        name = "tool1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${util1.out} ${util2.out} > $out && echo 'tool1 integration' >> $out" ];
      };

      tool2 = builtins.derivation {
        name = "tool2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${util2.out} ${util3.out} > $out && echo 'tool2 integration' >> $out" ];
      };

      tool3 = builtins.derivation {
        name = "tool3";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${util1.out} ${util3.out} > $out && echo 'tool3 integration' >> $out" ];
      };

      # Layer 4 - depends on layer 3
      service1 = builtins.derivation {
        name = "service1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${tool1.out} ${tool2.out} > $out && echo 'service1 implementation' >> $out" ];
      };

      service2 = builtins.derivation {
        name = "service2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${tool2.out} ${tool3.out} > $out && echo 'service2 implementation' >> $out" ];
      };

      service3 = builtins.derivation {
        name = "service3";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${tool1.out} ${tool3.out} > $out && echo 'service3 implementation' >> $out" ];
      };

      # Layer 5 - depends on layer 4
      app1 = builtins.derivation {
        name = "app1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${service1.out} ${service2.out} > $out && echo 'app1 assembly' >> $out" ];
      };

      app2 = builtins.derivation {
        name = "app2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${service2.out} ${service3.out} > $out && echo 'app2 assembly' >> $out" ];
      };

      app3 = builtins.derivation {
        name = "app3";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${service1.out} ${service3.out} > $out && echo 'app3 assembly' >> $out" ];
      };

      # Layer 6 - depends on layer 5
      system1 = builtins.derivation {
        name = "system1";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${app1.out} ${app2.out} > $out && echo 'system1 orchestration' >> $out" ];
      };

      system2 = builtins.derivation {
        name = "system2";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${app2.out} ${app3.out} > $out && echo 'system2 orchestration' >> $out" ];
      };

      # Layer 7 - final integration
      platform = builtins.derivation {
        name = "platform";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${system1.out} ${system2.out} > $out && echo 'platform complete' >> $out" ];
      };

      # Final deployment - depends on everything
      deployment = builtins.derivation {
        name = "deployment";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo ${platform.out} ${app1.out} ${service1.out} ${tool1.out} ${util1.out} ${lib1.out} ${base.out} > $out && echo 'deployment ready' >> $out" ];
      };

      buildWait5Sec = builtins.derivation {
        name = "buildWait5Sec";
        system = "x86_64-linux";
        builder = "/bin/sh";
        args = [ "-c" "echo \"Hello World!\" > $out" ];
      };

    in {
      # Expose all 20 derivations + legacy
      inherit base lib1 lib2 util1 util2 util3 tool1 tool2 tool3 
              service1 service2 service3 app1 app2 app3 system1 system2 
              platform deployment buildWait5Sec;
    };
  };
}
