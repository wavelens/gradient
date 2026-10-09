{ stdenv
, typst
, ffmpeg-headless
, python3
}: stdenv.mkDerivation {
  pname = "gradient-logo";
  version = "1.0.0";
  src = ../../logo;

  nativeBuildInputs = [
    ffmpeg-headless
    (python3.withPackages (ps: with ps; [ picosvg skia-pathops ]))
    (typst.withPackages (ps:
      with ps; [
        cetz_0_5_2
      ]
    ))
  ];

  buildPhase = ''
    mkdir "$out"
    typst compile logo.typ $out/logo.svg
    python3 transparent.py $out/logo.svg $out/logo-transparent.svg $out/logo-outlined.svg

    for i in {0..60}; do
      printf -v numberstring '%02d' "$i"
      time=$(awk -v i="$i" 'BEGIN { printf "%.2f", i / 60 }')
      echo "$time $numberstring"
      typst compile --ppi 5000 --input time="$time" logo.typ "logo$numberstring.png"
    done

    ls -lah
    cp logo60.png $out/logo.png
    ffmpeg \
      -framerate 24 \
      -i "logo%02d.png" \
      -filter_complex "split[s0][s1];[s0]palettegen[p];[s1][p]paletteuse" \
      -loop 0 \
      "$out/animation.gif"
  '';
}
