// EagleView's compact standard library: the names models use most from the
// SysML v2 Systems Model Library, so that they resolve and can be navigated.
// Declarations with one-line descriptions; their meaning is given by the SysML v2 specification.
standard library package Base {
    doc /* The most general types: everything specializes these. */
    abstract part def Anything {
        doc /* The most general part: every part, item and occurrence is one. */
    }
    abstract attribute def DataValue {
        doc /* The most general attribute type: values that have no identity or lifetime, such as numbers, strings and quantity values. */
    }
    attribute dataValues : DataValue [*] nonunique;
    part things : Anything [*] nonunique;
}

standard library package Occurrences {
    private import Base::*;
    abstract occurrence def Occurrence {
        doc /* Something that exists in time: it has a lifetime with a start and an end, and can be split into snapshots and time slices. */
    }
    occurrence occurrences : Occurrence [*] nonunique;
    abstract occurrence def Life :> Occurrence;
}

standard library package Items {
    private import Occurrences::*;
    abstract item def Item :> Occurrence {
        doc /* An occurrence that can be stored, moved or flow between parts, such as fuel, a signal or data. */
    }
    item items : Item [*] nonunique;
}

standard library package Parts {
    private import Items::*;
    abstract part def Part :> Item {
        doc /* A thing of the system or its environment that has structure: a system, subsystem or component. Parts are items. */
    }
    part parts : Part [*] nonunique;
}

standard library package Ports {
    abstract port def Port {
        doc /* A connection point on the boundary of a part, through which items flow and with which other parts interact. */
    }
    port ports : Port [*] nonunique;
}

standard library package Connections {
    abstract connection def Connection {
        doc /* A link between parts or ports, itself an occurrence that can carry information about the link. */
    }
    abstract connection def BinaryConnection :> Connection;
    connection connections : Connection [*] nonunique;
}

standard library package Interfaces {
    private import Connections::*;
    abstract interface def Interface :> Connection {
        doc /* A connection between ports, typed by an interface definition that says which ends it joins. */
    }
    abstract interface def BinaryInterface :> Interface, BinaryConnection;
    interface interfaces : Interface [*] nonunique;
}

standard library package Allocations {
    private import Connections::*;
    abstract allocation def Allocation :> BinaryConnection {
        doc /* A connection saying that one element (often an action) is realized by another (often a part). */
    }
    allocation allocations : Allocation [*] nonunique;
}

standard library package Actions {
    private import Occurrences::*;
    abstract action def Action :> Occurrence {
        doc /* Behavior that happens over time: it takes inputs, produces outputs and is made of steps and successions. */
    }
    action actions : Action [*] nonunique;
    action def AcceptAction :> Action;
    action def SendAction :> Action;
    action def DecisionAction :> Action;
    action def MergeAction :> Action;
    action def ForkAction :> Action;
    action def JoinAction :> Action;
}

standard library package States {
    private import Actions::*;
    abstract state def StateAction :> Action {
        doc /* The behavior of a state: what happens while the state is active. */
    }
    state stateActions : StateAction [*] nonunique;
}

standard library package Calculations {
    private import Actions::*;
    abstract calc def Calculation :> Action {
        doc /* An action that computes a result from its inputs, with no other effect. */
    }
    calc calculations : Calculation [*] nonunique;
}

standard library package Constraints {
    abstract constraint def ConstraintCheck {
        doc /* A Boolean expression that is either satisfied or not for given values. */
    }
    constraint constraintChecks : ConstraintCheck [*] nonunique;
}

standard library package Requirements {
    private import Constraints::*;
    abstract requirement def RequirementCheck :> ConstraintCheck {
        doc /* A constraint that a subject must satisfy, with the assumptions under which it applies. */
    }
    requirement requirementChecks : RequirementCheck [*] nonunique;
    abstract requirement def ConcernCheck :> RequirementCheck;
    abstract requirement def DesignConstraintCheck :> RequirementCheck;
    abstract requirement def FunctionalRequirementCheck :> RequirementCheck;
    abstract requirement def InterfaceRequirementCheck :> RequirementCheck;
    abstract requirement def PerformanceRequirementCheck :> RequirementCheck;
    abstract requirement def PhysicalRequirementCheck :> RequirementCheck;
}

standard library package Cases {
    private import Calculations::*;
    abstract case def Case :> Calculation {
        doc /* A calculation with a subject and an objective, the base of analysis, verification and use cases. */
    }
    case cases : Case [*] nonunique;
}

standard library package AnalysisCases {
    private import Cases::*;
    abstract analysis def AnalysisCase :> Case {
        doc /* A case that analyzes its subject and returns the result of the analysis. */
    }
    analysis analysisCases : AnalysisCase [*] nonunique;
}

standard library package VerificationCases {
    private import Cases::*;
    abstract verification def VerificationCase :> Case {
        doc /* A case that checks whether its subject meets requirements, returning a verdict. */
    }
    verification verificationCases : VerificationCase [*] nonunique;
    enum def VerdictKind {
        enum pass;
        enum fail;
        enum inconclusive;
        enum error;
    }
}

standard library package UseCases {
    private import Cases::*;
    abstract use case def UseCase :> Case {
        doc /* A case describing how actors use the subject to achieve a goal. */
    }
    use case useCases : UseCase [*] nonunique;
}

standard library package Views {
    abstract view def View {
        doc /* A presentation of selected model elements that frames the concerns of a viewpoint. */
    }
    abstract viewpoint def Viewpoint {
        doc /* The concerns of stakeholders that a view must address. */
    }
    abstract rendering def Rendering {
        doc /* How a view is presented: as a diagram, table, text, ... */
    }
}

standard library package Metadata {
    abstract metadata def MetadataItem {
        doc /* Base of metadata definitions: data attached to model elements with @ or #. */
    }
}
