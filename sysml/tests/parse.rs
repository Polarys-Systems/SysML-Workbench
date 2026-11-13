use sysml::*;

const DRONE: &str = include_str!("data/drone.sysml");

fn parse_ok(src: &str) -> Model {
    let r = parse(src);
    if !r.diagnostics.is_empty() {
        let lines = LineIndex::new(src);
        for d in &r.diagnostics {
            let (l, c) = lines.line_col(d.span.start);
            eprintln!("{l}:{c}: {d}");
        }
        panic!("unexpected diagnostics");
    }
    r.model
}

fn find<'a>(members: &'a [Member], name: &str) -> &'a Member {
    members
        .iter()
        .find(|m| m.kind.effective_name() == Some(name))
        .unwrap_or_else(|| panic!("no member named {name}"))
}

fn usage(m: &Member) -> &Usage {
    match &m.kind {
        MemberKind::Usage(u) => u,
        other => panic!("not a usage: {other:?}"),
    }
}

fn definition(m: &Member) -> &Definition {
    match &m.kind {
        MemberKind::Definition(d) => d,
        other => panic!("not a definition: {other:?}"),
    }
}

fn expr(src: &str) -> String {
    let model = parse_ok(&format!("attribute x = {src};"));
    usage(&model.members[0]).value.as_ref().unwrap().expr.to_string()
}

#[test]
fn sample_model_parses_cleanly() {
    let model = parse_ok(DRONE);
    assert_eq!(model.members.len(), 1);
    let MemberKind::Package(pkg) = &model.members[0].kind else { panic!() };
    assert_eq!(pkg.id.name.as_ref().unwrap().text, "DroneSystem");

    let MemberKind::Comment(doc) = &pkg.body[0].kind else { panic!() };
    assert_eq!((doc.kind, doc.text.as_str()), (CommentKind::Doc, "A small delivery drone."));

    let MemberKind::Import(imp) = &pkg.body[1].kind else { panic!() };
    assert_eq!(pkg.body[1].visibility, Some(Visibility::Private));
    assert!(imp.is_namespace && !imp.is_recursive);
    let MemberKind::Import(imp) = &pkg.body[2].kind else { panic!() };
    assert!(!imp.is_namespace && imp.is_recursive);
}

#[test]
fn definitions_and_usages() {
    let model = parse_ok(DRONE);
    let pkg = model.members[0].kind.body();

    let motor = definition(find(pkg, "Motor"));
    assert_eq!(motor.kind, DefKind::Part);
    assert_eq!(motor.specializes[0].to_string(), "Component");
    let power_in = usage(find(&motor.body, "powerIn"));
    assert_eq!(power_in.kind, UsageKind::Port);
    assert!(power_in.typed_by[0].conjugated);
    let rpm = usage(find(&motor.body, "rpm"));
    assert_eq!(rpm.multiplicity.as_ref().unwrap().to_string(), "[0..1]");

    assert!(definition(find(pkg, "Component")).is_abstract);

    let colors = definition(find(pkg, "FlightMode"));
    let values: Vec<_> = colors.body.iter().map(|m| (usage(m).kind, m.kind.effective_name())).collect();
    assert_eq!(
        values,
        [
            (UsageKind::Enum, Some("idle")),
            (UsageKind::Enum, Some("hover")),
            (UsageKind::Enum, Some("cruise"))
        ]
    );

    let drone = definition(find(pkg, "Drone"));
    let battery = find(&drone.body, "battery");
    assert_eq!(battery.metadata[0].to_string(), "Safety");
    let motors = usage(find(&drone.body, "motors"));
    assert!(motors.is_ordered);
    assert_eq!(motors.multiplicity.as_ref().unwrap().to_string(), "[4]");
    let cargo = usage(find(&drone.body, "cargo"));
    assert!(cargo.prefix.is_ref && cargo.kind == UsageKind::Item);

    let mode = usage(find(&drone.body, "mode"));
    let value = mode.value.as_ref().unwrap();
    assert_eq!(value.kind, ValueKind::Initial);
    assert_eq!(value.expr.to_string(), "FlightMode::idle");

    let total = usage(find(&drone.body, "totalMass"));
    assert_eq!(
        total.value.as_ref().unwrap().expr.to_string(),
        "battery.mass + motors.mass->sum() [kg]"
    );

    let cap = usage(find(definition(find(pkg, "BatteryPack")).body.as_slice(), "capacity"));
    assert_eq!(cap.id.short_name.as_ref().unwrap().text, "cap");
    assert!(cap.value.as_ref().unwrap().is_default);
}

#[test]
fn connectors() {
    let model = parse_ok(DRONE);
    let drone = definition(find(model.members[0].kind.body(), "Drone"));

    let link = usage(find(&drone.body, "link"));
    assert_eq!(link.kind, UsageKind::Interface);
    let c = link.connector.as_ref().unwrap();
    assert_eq!(c.kind, ConnectorKind::Connect);
    let ends: Vec<_> = c.ends.iter().map(|e| e.target.to_string()).collect();
    assert_eq!(ends, ["battery.powerOut", "motors.powerIn"]);

    let connectors: Vec<_> = drone
        .body
        .iter()
        .filter_map(|m| match &m.kind {
            MemberKind::Usage(u) => u.connector.as_ref().map(|c| (u.kind, c.kind)),
            _ => None,
        })
        .collect();
    assert_eq!(
        connectors,
        [
            (UsageKind::Interface, ConnectorKind::Connect),
            (UsageKind::Binding, ConnectorKind::Bind),
            (UsageKind::Flow, ConnectorKind::Flow),
        ]
    );

    let perform = usage(find(&drone.body, "deliver"));
    assert_eq!((perform.prefix.role, perform.kind), (Some(Role::Perform), UsageKind::Action));

    let satisfy = model.members[0]
        .kind
        .body()
        .iter()
        .find_map(|m| match &m.kind {
            MemberKind::Usage(u) if u.prefix.role == Some(Role::Satisfy) => Some(u),
            _ => None,
        })
        .unwrap();
    assert_eq!(satisfy.references[0].to_string(), "massReq");
    assert_eq!(satisfy.by.as_ref().unwrap().to_string(), "drone1");
}

#[test]
fn actions_and_states() {
    let model = parse_ok(DRONE);
    let pkg = model.members[0].kind.body();

    let deliver = definition(find(pkg, "Deliver"));
    let payload = usage(find(&deliver.body, "payload"));
    assert_eq!(payload.prefix.direction, Some(Direction::In));
    assert!(matches!(&deliver.body[3].kind, MemberKind::Then(m) if m.kind.effective_name() == Some("takeOff")));
    let guards = deliver
        .body
        .iter()
        .filter(|m| matches!(&m.kind, MemberKind::Transition(t) if t.guard.is_some() || t.is_else))
        .count();
    assert_eq!(guards, 2);

    let states = definition(find(pkg, "FlightStates"));
    let lift_off = match &find(&states.body, "liftOff").kind {
        MemberKind::Transition(t) => t,
        _ => panic!(),
    };
    assert_eq!(lift_off.source.as_ref().unwrap().to_string(), "grounded");
    assert_eq!(lift_off.target.as_ref().unwrap().to_string(), "airborne");
    let trigger = lift_off.trigger.as_ref().unwrap();
    assert!(matches!(&trigger.kind, TriggerKind::Signal { ty: Some(t), .. } if t.to_string() == "Launch"));
    assert_eq!(trigger.via.as_ref().unwrap().to_string(), "drone1.battery.powerOut");
    assert_eq!(lift_off.guard.as_ref().unwrap().to_string(), "drone1.mode == FlightMode::idle");
    assert!(matches!(&lift_off.effect, Some(ActionRef::Inline(m)) if matches!(m.kind, MemberKind::Send(_))));

    let airborne = usage(find(&states.body, "airborne"));
    let actions: Vec<_> = airborne
        .body
        .iter()
        .map(|m| match &m.kind {
            MemberKind::StateAction(a) => a.kind,
            _ => panic!(),
        })
        .collect();
    assert_eq!(actions, [StateActionKind::Entry, StateActionKind::Do, StateActionKind::Exit]);

    let transitions = states.body.iter().filter(|m| matches!(m.kind, MemberKind::Transition(_))).count();
    assert_eq!(transitions, 3);
}

#[test]
fn requirements_and_calcs() {
    let model = parse_ok(DRONE);
    let pkg = model.members[0].kind.body();

    let req = definition(find(pkg, "MassLimit"));
    assert_eq!(req.id.short_name.as_ref().unwrap().text, "R1");
    let subject = usage(find(&req.body, "drone"));
    assert_eq!(subject.kind, UsageKind::Subject);
    let require = req
        .body
        .iter()
        .find_map(|m| match &m.kind {
            MemberKind::Usage(u) if u.prefix.role == Some(Role::Require) => Some(u),
            _ => None,
        })
        .unwrap();
    assert!(matches!(&require.body[0].kind, MemberKind::Expression(e) if e.to_string() == "drone.totalMass <= limit"));

    let calc = definition(find(pkg, "Endurance"));
    let MemberKind::Expression(result) = &calc.body.last().unwrap().kind else { panic!() };
    assert_eq!(result.to_string(), "if power > 0 ? capacity / power else 0.0");
    assert_eq!(usage(&calc.body[2]).kind, UsageKind::Return);
}

#[test]
fn expressions_print_with_minimal_parentheses() {
    assert_eq!(expr("1 + 2 * 3"), "1 + 2 * 3");
    assert_eq!(expr("(1 + 2) * 3"), "(1 + 2) * 3");
    assert_eq!(expr("2 ** 3 ** 2"), "2 ** 3 ** 2");
    assert_eq!(expr("(2 ** 3) ** 2"), "(2 ** 3) ** 2");
    assert_eq!(expr("a - (b - c)"), "a - (b - c)");
    assert_eq!(expr("-x.y"), "-x.y");
    assert_eq!(expr("9.81 [m/s^2]"), "9.81 [m / s ** 2]");
    assert_eq!(expr("f(a = 1, 2)"), "f(a = 1, 2)");
    assert_eq!(expr("x istype T and y as U == z"), "x istype T and y as U == z");
    assert_eq!(expr("(1, 'odd name', \"s\")"), "(1, 'odd name', \"s\")");
    assert_eq!(expr("v#(1) ?? null"), "v#(1) ?? null");
}

#[test]
fn recovers_from_errors() {
    let src = "package P {\n  part def A;\n  part x : = ;\n  part def B { attribute a : ; attribute b; }\n  part def C;\n}\n part def D;";
    let r = parse(src);
    assert_eq!(r.diagnostics.len(), 2, "{:?}", r.diagnostics);
    let lines = LineIndex::new(src);
    assert_eq!(lines.line_col(r.diagnostics[0].span.start).0, 3);
    let pkg = r.model.members[0].kind.body();
    let names: Vec<_> = pkg.iter().filter_map(|m| m.kind.effective_name()).collect();
    assert_eq!(names, ["A", "B", "C"]);
    assert_eq!(r.model.members[1].kind.effective_name(), Some("D"));
}

#[test]
fn unclosed_body_keeps_members() {
    let r = parse("package P { part def A; part def B;");
    assert_eq!(r.diagnostics.len(), 1);
    assert_eq!(r.model.members[0].kind.body().len(), 2);
}

#[test]
fn index_and_resolution() {
    let model = parse_ok(DRONE);
    let index = Index::build(&model);

    let drone = index.get("DroneSystem::Drone").unwrap();
    assert!(index.get("DroneSystem::Drone::battery").is_some());
    // `:>> battery` is indexed under the redefined name.
    assert!(index.get("DroneSystem::drone1::battery::mass").is_some());

    let qn = |s: &str| {
        let m = parse_ok(&format!("attribute x : {s};"));
        usage(&m.members[0]).typed_by[0].name.clone()
    };
    // Lexical lookup from an inner scope.
    let motor = index.resolve(Some(drone), &qn("Motor")).unwrap();
    assert_eq!(index.entry(motor).qualified_name, "DroneSystem::Motor");
    // Through `import Units::**`.
    let mass = index.resolve(Some(drone), &qn("MassValue")).unwrap();
    assert_eq!(index.entry(mass).qualified_name, "DroneSystem::Units::MassValue");
    // Through an alias.
    let kg = index.resolve(Some(drone), &qn("Kg")).unwrap();
    assert_eq!(index.entry(kg).qualified_name, "DroneSystem::Units::kg");
    assert!(index.resolve(Some(drone), &qn("Missing")).is_none());
}
