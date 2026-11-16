use sysml::eval::{Evaluator, Problem, Report, Status, Verdict, evaluate};
use sysml::*;

const DRONE: &str = include_str!("data/drone.sysml");

const BUDGET: &str = r#"
package Budget {
    part def Component {
        attribute mass;
        attribute power = 0 [W];
    }
    part def Motor :> Component {
        :>> mass = 150 [g];
        :>> power = 120 [W];
    }
    part def Battery :> Component {
        attribute capacity = 5 [Ah];
        attribute voltage = 14.8 [V];
        :>> mass = 0.6 [kg];
    }
    part def Airframe :> Component;
    enum def Mode { enum idle; enum cruise; }

    part def Drone {
        part airframe : Airframe;
        part battery : Battery;
        part motors : Motor [4];
        attribute mode = Mode::cruise;
        attribute speed = if mode == Mode::cruise ? 54 [km/h] else 0 [m/s];
        attribute totalMass = airframe.mass + battery.mass + motors.mass->sum() [kg];
        attribute totalPower = motors.power->sum();
        attribute energy = (battery.capacity * battery.voltage) [Wh];
        attribute endurance = Endurance(energy, totalPower) [min];
    }

    calc def Endurance {
        in energy : Real;
        in power : Real;
        energy / power
    }

    part light : Drone { :>> airframe { :>> mass = 0.4 [kg]; } }
    part heavy : Drone { :>> airframe { :>> mass = 3.9 [kg]; } }

    requirement def MassLimit {
        subject d : Drone;
        attribute limit;
        require constraint { d.totalMass <= limit }
    }
    requirement mass5 : MassLimit { :>> limit = 5 [kg]; }

    satisfy mass5 by light;
    satisfy mass5 by heavy;
}
"#;

fn parse_ok(src: &str) -> Model {
    let r = parse(src);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    r.model
}

fn value(src: &str, element: &str, path: &[&str]) -> Result<String, Problem> {
    let model = parse_ok(src);
    let index = Index::build(&model);
    Evaluator::new(&index).value_of(element, path).map(|v| v.to_string())
}

fn report(src: &str) -> Report {
    let model = parse_ok(src);
    let index = Index::build(&model);
    evaluate(&model, &index)
}

#[test]
fn rolls_up_values_through_instances() {
    let v = |e: &str, p: &[&str]| value(BUDGET, e, p).unwrap();
    assert_eq!(v("Budget::light", &["totalMass"]), "1.6 kg");
    assert_eq!(v("Budget::heavy", &["totalMass"]), "5.1 kg");
    assert_eq!(v("Budget::light", &["airframe", "mass"]), "0.4 kg");
    assert_eq!(v("Budget::light", &["motors", "mass"]), "(150 g, 150 g, 150 g, 150 g)");
    assert_eq!(v("Budget::light", &["totalPower"]), "480 W");
    assert_eq!(v("Budget::light", &["energy"]), "74 Wh");
    assert_eq!(v("Budget::light", &["endurance"]), "9.25 min");
    assert_eq!(v("Budget::light", &["mode"]), "Mode::cruise");
    assert_eq!(v("Budget::light", &["speed"]), "54 km/h");
}

#[test]
fn checks_requirements() {
    let r = report(BUDGET);
    assert!(r.diagnostics.is_empty(), "{:?}", r.diagnostics);
    let check = |subject: &str| r.checks.iter().find(|c| c.subject == subject).unwrap();
    assert_eq!(check("Budget::light").verdict, Verdict::Satisfied);
    assert_eq!(check("Budget::light").detail, "1.6 kg <= 5 kg");
    assert_eq!(check("Budget::heavy").verdict, Verdict::Violated);
    assert_eq!(check("Budget::heavy").detail, "5.1 kg <= 5 kg");
    assert_eq!(check("Budget::heavy").requirement, "Budget::mass5");
}

#[test]
fn lists_instance_values() {
    let r = report(BUDGET);
    let light = r.instances.iter().find(|i| i.qualified_name == "Budget::light").unwrap();
    let get = |n: &str| light.values.iter().find(|v| v.name == n).unwrap();
    assert_eq!(get("totalMass").text, "1.6 kg");
    assert_eq!(get("endurance").text, "9.25 min");
    // Inherited from Component, not overridden in Drone.
    assert!(light.values.iter().all(|v| v.name != "mass"));
}

#[test]
fn reports_unit_errors() {
    let src = BUDGET.replace(
        "attribute totalPower = motors.power->sum();",
        "attribute totalPower = motors.power->sum();\n        attribute bad = battery.mass + totalPower;\n        attribute late = battery.capacity * battery.voltage [Wh];",
    );
    let r = report(&src);
    let messages: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(messages.contains(&"cannot add kg and W: their units differ"), "{messages:?}");
    // `[Wh]` binds to `battery.voltage` alone.
    assert!(messages.contains(&"a value in V cannot be given in Wh"), "{messages:?}");
    assert!(r.notes.iter().any(|n| n.status == Status::Error));
}

#[test]
fn reports_cycles_and_unknown_units() {
    let r = report("package P { part def A { attribute a = b + 1; attribute b = a * 2; attribute c = 3 [furlong]; } }");
    let messages: Vec<&str> = r.diagnostics.iter().map(|d| d.message.as_str()).collect();
    assert!(messages.iter().any(|m| m.ends_with("depends on itself")), "{messages:?}");
    assert!(messages.contains(&"unknown unit `furlong`"), "{messages:?}");
}

#[test]
fn calls_constraints_and_builtins() {
    let src = "package P {
        constraint def Positive { in x : Real; x > 0 }
        part def T {
            attribute ok = Positive(3);
            attribute side = sqrt(16 [m^2]);
            attribute top = max(2 [m], 300 [cm]);
        }
    }";
    assert_eq!(value(src, "P::T", &["ok"]).unwrap(), "true");
    assert_eq!(value(src, "P::T", &["side"]).unwrap(), "4 m");
    assert_eq!(value(src, "P::T", &["top"]).unwrap(), "300 cm");
}

#[test]
fn sample_model_explains_missing_values() {
    let model = parse(DRONE).model;
    let index = Index::build(&model);
    let r = evaluate(&model, &index);
    let check = r.checks.iter().find(|c| c.requirement == "DroneSystem::massReq").unwrap();
    assert_eq!(check.subject, "DroneSystem::drone1");
    assert_eq!(check.verdict, Verdict::Unknown);
    assert_eq!(check.detail, "drone1.motors.mass has no value");
    let drone1 = r.instances.iter().find(|i| i.qualified_name == "DroneSystem::drone1").unwrap();
    let total = drone1.values.iter().find(|v| v.name == "totalMass").unwrap();
    assert_eq!(total.status, Status::Unknown);
}

/// Bundling the standard library must not change what the sample computes.
#[test]
fn library_leaves_sample_values_unchanged() {
    let src = include_str!("data/drone.sysml");
    let model = sysml::parse(src).model;
    let alone = sysml::eval::evaluate(&model, &sysml::Index::build(&model));
    let mut models = vec![&model];
    models.extend(sysml::library::files().iter().map(|f| &f.model));
    let index = sysml::Index::build_many(&models);
    let with_lib = sysml::eval::evaluate_source(&model, &index, 0);
    let notes = |r: &sysml::eval::Report| r.notes.iter().map(|n| (n.span, n.text.clone())).collect::<Vec<_>>();
    assert_eq!(notes(&alone), notes(&with_lib));
    let verdicts = |r: &sysml::eval::Report| r.checks.iter().map(|c| (c.verdict, c.detail.clone())).collect::<Vec<_>>();
    assert_eq!(verdicts(&alone), verdicts(&with_lib));
    assert_eq!(alone.diagnostics.len(), with_lib.diagnostics.len());
}

/// Where a part's multiplicity and a redefined value can come from, in the
/// sample's shape: the motor mass set on the definition, on the usage in
/// `Drone`, or on the instance.
#[test]
fn sample_rolls_up_once_motor_mass_is_given() {
    let cases = [
        ("part def Motor :> Component {", "part def Motor :> Component { :>> mass = 0.3 [kg];"),
        ("part motors : Motor [4] ordered;", "part motors : Motor [4] ordered { :>> mass = 0.3 [kg]; }"),
        (":>> battery {", ":>> motors { :>> mass = 0.3 [kg]; }\n        :>> battery {"),
    ];
    for (from, to) in cases {
        let src = DRONE.replacen(from, to, 1);
        assert_ne!(src, DRONE, "{from}");
        let model = parse(&src).model;
        let index = Index::build(&model);
        let ev = Evaluator::new(&index);
        assert_eq!(ev.value_of("DroneSystem::drone1", &["totalMass"]).map(|v| v.to_string()), Ok("2.4 kg".into()), "{to}");
        let r = evaluate(&model, &index);
        let check = r.checks.iter().find(|c| c.requirement == "DroneSystem::massReq").unwrap();
        assert_eq!(check.verdict, Verdict::Satisfied, "{to}: {}", check.detail);
    }
}
