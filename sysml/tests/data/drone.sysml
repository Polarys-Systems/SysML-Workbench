// Sample model exercising the supported notation.
package DroneSystem {
    doc /* A small delivery drone. */

    private import ScalarValues::*;
    import Units::**;
    alias Kg for Units::kg;

    package Units {
        attribute def MassValue;
        attribute def PowerValue;
        attribute kg;
    }

    metadata def Safety {
        attribute critical : Boolean;
    }

    enum def FlightMode {
        enum idle;
        hover = 1;
        cruise;
    }

    attribute def Battery :> Units::PowerValue;

    item def Payload;

    port def PowerPort {
        out item power : Units::PowerValue;
    }

    abstract part def Component {
        attribute mass : Units::MassValue;
    }

    part def Motor :> Component {
        port powerIn : ~PowerPort;
        attribute rpm : Real [0..1];
    }

    part def BatteryPack :> Component {
        port powerOut : PowerPort;
        attribute <cap> capacity : Real default = 5000.0;
    }

    interface def PowerLink {
        end port source : PowerPort;
        end port sink : ~PowerPort;
    }

    part def Drone {
        /* The drone's body. */
        #Safety part battery : BatteryPack;
        part motors : Motor [4] ordered;
        ref item cargo : Payload [0..1];
        attribute mode : FlightMode := FlightMode::idle;
        attribute totalMass = battery.mass + motors.mass->sum() [kg];

        interface link : PowerLink connect battery.powerOut to motors.powerIn;
        bind battery.mass = cargo.mass;
        flow of Units::PowerValue from battery.powerOut to motors.powerIn;

        perform action deliver : Deliver;
        exhibit state flight : FlightStates;
    }

    part drone1 : Drone {
        :>> battery {
            :>> mass = 1.2 [kg];
        }
        @Safety { :>> critical = true; }
    }

    action def Deliver {
        in item payload : Payload;
        out attribute success : Boolean;

        first start;
        then action takeOff;
        then action fly {
            in target : Real;
        }
        then land;
        action land;
        first fly then land;
        decide;
            if success then land;
            else takeOff;
    }

    state def FlightStates {
        entry; then grounded;

        state grounded;
        state airborne {
            entry action spinUp;
            do monitor;
            exit action { }
        }

        transition liftOff
            first grounded
            accept Launch via drone1.battery.powerOut
            if drone1.mode == FlightMode::idle
            do send Ack() to drone1
            then airborne;

        accept Land then grounded;
        transition airborne accept after 30 [s] then grounded;
    }

    requirement def <'R1'> MassLimit {
        doc /* The drone shall weigh at most 5 kg. */
        subject drone : Drone;
        attribute limit : Units::MassValue;
        require constraint { drone.totalMass <= limit }
    }

    requirement massReq : MassLimit {
        :>> limit = 5 [kg];
    }

    satisfy massReq by drone1;

    calc def Endurance {
        in capacity : Real;
        in power : Real;
        return : Real;
        if power > 0 ? capacity / power else 0.0
    }

    constraint def Positive {
        in x : Real;
        x > 0 and not (x == 0)
    }

    dependency from Drone to Units;

    comment about Drone, Deliver /* Review before release. */
    dependency from weight to MassLimit;
}
