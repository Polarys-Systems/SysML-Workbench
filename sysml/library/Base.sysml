// EagleView's compact standard library: the names models use most from the
// SysML v2 Systems Model Library, so that they resolve and can be navigated.
// Declarations only; their meaning is given by the SysML v2 specification.
standard library package Base {
    doc /* The most general types: everything specializes these. */
    abstract part def Anything;
    abstract attribute def DataValue;
    attribute dataValues : DataValue [*] nonunique;
    part things : Anything [*] nonunique;
}

standard library package Occurrences {
    private import Base::*;
    abstract occurrence def Occurrence;
    occurrence occurrences : Occurrence [*] nonunique;
    abstract occurrence def Life :> Occurrence;
}

standard library package Items {
    private import Occurrences::*;
    abstract item def Item :> Occurrence;
    item items : Item [*] nonunique;
}

standard library package Parts {
    private import Items::*;
    abstract part def Part :> Item;
    part parts : Part [*] nonunique;
}

standard library package Ports {
    abstract port def Port;
    port ports : Port [*] nonunique;
}

standard library package Connections {
    abstract connection def Connection;
    abstract connection def BinaryConnection :> Connection;
    connection connections : Connection [*] nonunique;
}

standard library package Interfaces {
    private import Connections::*;
    abstract interface def Interface :> Connection;
    abstract interface def BinaryInterface :> Interface, BinaryConnection;
    interface interfaces : Interface [*] nonunique;
}

standard library package Allocations {
    private import Connections::*;
    abstract allocation def Allocation :> BinaryConnection;
    allocation allocations : Allocation [*] nonunique;
}

standard library package Actions {
    private import Occurrences::*;
    abstract action def Action :> Occurrence;
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
    abstract state def StateAction :> Action;
    state stateActions : StateAction [*] nonunique;
}

standard library package Calculations {
    private import Actions::*;
    abstract calc def Calculation :> Action;
    calc calculations : Calculation [*] nonunique;
}

standard library package Constraints {
    abstract constraint def ConstraintCheck;
    constraint constraintChecks : ConstraintCheck [*] nonunique;
}

standard library package Requirements {
    private import Constraints::*;
    abstract requirement def RequirementCheck :> ConstraintCheck;
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
    abstract case def Case :> Calculation;
    case cases : Case [*] nonunique;
}

standard library package AnalysisCases {
    private import Cases::*;
    abstract analysis def AnalysisCase :> Case;
    analysis analysisCases : AnalysisCase [*] nonunique;
}

standard library package VerificationCases {
    private import Cases::*;
    abstract verification def VerificationCase :> Case;
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
    abstract use case def UseCase :> Case;
    use case useCases : UseCase [*] nonunique;
}

standard library package Views {
    abstract view def View;
    abstract viewpoint def Viewpoint;
    abstract rendering def Rendering;
}

standard library package Metadata {
    abstract metadata def MetadataItem;
}
