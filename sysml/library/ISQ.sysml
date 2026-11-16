standard library package ISQ {
    doc /* International System of Quantities (ISO 80000): the value types attributes are typed with. */
    private import Quantities::*;
    private import MeasurementReferences::*;

    // Base quantities
    attribute def LengthValue :> ScalarQuantityValue;
    attribute length : LengthValue :> scalarQuantities;
    attribute def LengthUnit :> SimpleUnit;
    attribute def MassValue :> ScalarQuantityValue;
    attribute mass : MassValue :> scalarQuantities;
    attribute def MassUnit :> SimpleUnit;
    attribute def DurationValue :> ScalarQuantityValue;
    attribute duration : DurationValue :> scalarQuantities;
    attribute def DurationUnit :> SimpleUnit;
    attribute def ElectricCurrentValue :> ScalarQuantityValue;
    attribute electricCurrent : ElectricCurrentValue :> scalarQuantities;
    attribute def ElectricCurrentUnit :> SimpleUnit;
    attribute def ThermodynamicTemperatureValue :> ScalarQuantityValue;
    attribute thermodynamicTemperature : ThermodynamicTemperatureValue :> scalarQuantities;
    attribute def ThermodynamicTemperatureUnit :> SimpleUnit;
    attribute def AmountOfSubstanceValue :> ScalarQuantityValue;
    attribute amountOfSubstance : AmountOfSubstanceValue :> scalarQuantities;
    attribute def AmountOfSubstanceUnit :> SimpleUnit;
    attribute def LuminousIntensityValue :> ScalarQuantityValue;
    attribute luminousIntensity : LuminousIntensityValue :> scalarQuantities;
    attribute def LuminousIntensityUnit :> SimpleUnit;

    // Space and time
    attribute def AreaValue :> ScalarQuantityValue;
    attribute area : AreaValue :> scalarQuantities;
    attribute def AreaUnit :> DerivedUnit;
    attribute def VolumeValue :> ScalarQuantityValue;
    attribute volume : VolumeValue :> scalarQuantities;
    attribute def VolumeUnit :> DerivedUnit;
    attribute def SpeedValue :> ScalarQuantityValue;
    attribute speed : SpeedValue :> scalarQuantities;
    attribute def SpeedUnit :> DerivedUnit;
    attribute def AccelerationValue :> ScalarQuantityValue;
    attribute acceleration : AccelerationValue :> scalarQuantities;
    attribute def AccelerationUnit :> DerivedUnit;
    attribute def FrequencyValue :> ScalarQuantityValue;
    attribute frequency : FrequencyValue :> scalarQuantities;
    attribute def FrequencyUnit :> DerivedUnit;
    attribute def AngularMeasureValue :> ScalarQuantityValue;
    attribute angularMeasure : AngularMeasureValue :> scalarQuantities;
    attribute def AngularMeasureUnit :> DimensionOneUnit;
    attribute def AngularVelocityValue :> ScalarQuantityValue;
    attribute angularVelocity : AngularVelocityValue :> scalarQuantities;
    attribute def AngularVelocityUnit :> DerivedUnit;
    attribute def AngularAccelerationValue :> ScalarQuantityValue;
    attribute angularAcceleration : AngularAccelerationValue :> scalarQuantities;
    attribute def AngularAccelerationUnit :> DerivedUnit;

    // Mechanics
    attribute def ForceValue :> ScalarQuantityValue;
    attribute force : ForceValue :> scalarQuantities;
    attribute def ForceUnit :> DerivedUnit;
    attribute def TorqueValue :> ScalarQuantityValue;
    attribute torque : TorqueValue :> scalarQuantities;
    attribute def TorqueUnit :> DerivedUnit;
    attribute def PressureValue :> ScalarQuantityValue;
    attribute pressure : PressureValue :> scalarQuantities;
    attribute def PressureUnit :> DerivedUnit;
    attribute def EnergyValue :> ScalarQuantityValue;
    attribute energy : EnergyValue :> scalarQuantities;
    attribute def EnergyUnit :> DerivedUnit;
    attribute def PowerValue :> ScalarQuantityValue;
    attribute power : PowerValue :> scalarQuantities;
    attribute def PowerUnit :> DerivedUnit;
    attribute def MassDensityValue :> ScalarQuantityValue;
    attribute massDensity : MassDensityValue :> scalarQuantities;
    attribute def MassDensityUnit :> DerivedUnit;
    attribute def MassFlowRateValue :> ScalarQuantityValue;
    attribute massFlowRate : MassFlowRateValue :> scalarQuantities;
    attribute def MassFlowRateUnit :> DerivedUnit;
    attribute def VolumeFlowRateValue :> ScalarQuantityValue;
    attribute volumeFlowRate : VolumeFlowRateValue :> scalarQuantities;
    attribute def VolumeFlowRateUnit :> DerivedUnit;
    attribute def MomentOfInertiaValue :> ScalarQuantityValue;
    attribute momentOfInertia : MomentOfInertiaValue :> scalarQuantities;
    attribute def MomentOfInertiaUnit :> DerivedUnit;

    // Electromagnetism and light
    attribute def ElectricChargeValue :> ScalarQuantityValue;
    attribute electricCharge : ElectricChargeValue :> scalarQuantities;
    attribute def ElectricChargeUnit :> DerivedUnit;
    attribute def ElectricPotentialDifferenceValue :> ScalarQuantityValue;
    attribute electricPotentialDifference : ElectricPotentialDifferenceValue :> scalarQuantities;
    attribute def ElectricPotentialDifferenceUnit :> DerivedUnit;
    attribute def ResistanceValue :> ScalarQuantityValue;
    attribute resistance : ResistanceValue :> scalarQuantities;
    attribute def ResistanceUnit :> DerivedUnit;
    attribute def CapacitanceValue :> ScalarQuantityValue;
    attribute capacitance : CapacitanceValue :> scalarQuantities;
    attribute def CapacitanceUnit :> DerivedUnit;
    attribute def InductanceValue :> ScalarQuantityValue;
    attribute inductance : InductanceValue :> scalarQuantities;
    attribute def InductanceUnit :> DerivedUnit;
    attribute def MagneticFluxDensityValue :> ScalarQuantityValue;
    attribute magneticFluxDensity : MagneticFluxDensityValue :> scalarQuantities;
    attribute def MagneticFluxDensityUnit :> DerivedUnit;
    attribute def IlluminanceValue :> ScalarQuantityValue;
    attribute illuminance : IlluminanceValue :> scalarQuantities;
    attribute def IlluminanceUnit :> DerivedUnit;

    // Common other names
    alias TimeValue for DurationValue;
    alias time for duration;
    alias TimeUnit for DurationUnit;
    alias TemperatureValue for ThermodynamicTemperatureValue;
    alias temperature for thermodynamicTemperature;
    alias TemperatureUnit for ThermodynamicTemperatureUnit;
    alias VoltageValue for ElectricPotentialDifferenceValue;
    alias voltage for electricPotentialDifference;
    alias VoltageUnit for ElectricPotentialDifferenceUnit;
    alias DensityValue for MassDensityValue;
    alias density for massDensity;
    alias PlaneAngleValue for AngularMeasureValue;
    alias VelocityValue for SpeedValue;
    alias velocity for speed;
    alias distance for length;
    alias width for length;
    alias height for length;
    alias depth for length;
    alias radius for length;
    alias diameter for length;
    alias weight for force;
}
