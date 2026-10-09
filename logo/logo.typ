#import "@preview/cetz:0.5.2"
#let main_color = black;

//animation parameter 0..1
#let time = float(sys.inputs.at("time", default: "1"))

#set page(
  width: 12mm,
  height: 12mm,
)

// outline of the lambda
// from https://brand.nixos.org/logos/nixos-logo-black-flat-black-regular-horizontal-recommended.svg
#let lambda_path = (
  (624.0,              -249.41531628991848),
  (496.0,               -27.71281292110206),
  (-64.00000000000007, -997.6612651596733),
  (191.99999999999994, -997.6612651596733),
  (320.00000000000006, -775.9587617908571),
  (448.0000000000002,  -997.6612651596732),
  (576.0000000000002,  -997.6612651596733),
  (640.0000000000002,  -886.8100134752652),
  (448.00000000000006, -554.2562584220408),
)

//#let path = lambda_path.map(a => add_vecs(scale_vec(a, 0.001), (0.2,1)))
#let lambda() = cetz.draw.group( name: "lambda", {
  import cetz.draw: *
  stroke((
    //paint: main_color,
    //thickness: i,
    join: "round",
  ))
  scale(0.001)
  //move-to(x: .2, y: 1)
  move-to((.2, 1))
  line(
    ..lambda_path,
    close: true
  )
})

#let logo(range) = cetz.canvas({
  import cetz.draw: *
  stroke((
    join: "round",
  ))
  ortho(
    x: 5deg,
    y: 35deg,
    z: 0deg,
    {
    for i in range {
      on-xy(z: - i*1pt, {
        let r = calc.sqrt(i) * 0.6pt
        stroke((
          paint: main_color,
          thickness: r,
        ))
        lambda()
        stroke(0pt)
        fill(white)
        lambda()
      })
    }
    on-xy({
      stroke(1pt)
      fill(main_color)
      lambda()
    })
  })
  translate(x: -2cm)
})

#let stack_range(time) = {
  let scaled_time = time * 9
  let r = ()
  for i in range(0, 9, step: 3) {
    if i < scaled_time {
      r.push(i)
    }
  }
  r.push(scaled_time)
  r
}
#align(center+horizon, logo(stack_range(time)))

