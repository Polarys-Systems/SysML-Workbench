standard library package SI {
    doc /* SI units (ISO 80000), with their symbols as short names: `kg` is `kilogram`. */
    private import ISQ::*;
    private import MeasurementReferences::*;

    // Base units
    attribute <m> metre : LengthUnit;
    attribute <kg> kilogram : MassUnit;
    attribute <s> second : DurationUnit;
    attribute <A> ampere : ElectricCurrentUnit;
    attribute <K> kelvin : ThermodynamicTemperatureUnit;
    attribute <mol> mole : AmountOfSubstanceUnit;
    attribute <cd> candela : LuminousIntensityUnit;

    // Derived units with special names
    attribute <rad> radian : AngularMeasureUnit;
    attribute <sr> steradian : DimensionOneUnit;
    attribute <Hz> hertz : FrequencyUnit;
    attribute <N> newton : ForceUnit;
    attribute <Pa> pascal : PressureUnit;
    attribute <J> joule : EnergyUnit;
    attribute <W> watt : PowerUnit;
    attribute <C> coulomb : ElectricChargeUnit;
    attribute <V> volt : ElectricPotentialDifferenceUnit;
    attribute <F> farad : CapacitanceUnit;
    attribute <Ohm> ohm : ResistanceUnit;
    attribute <H> henry : InductanceUnit;
    attribute <T> tesla : MagneticFluxDensityUnit;
    attribute <lx> lux : IlluminanceUnit;
    attribute <degC> 'degree Celsius' : ThermodynamicTemperatureUnit;

    // Multiples and submultiples in common use
    attribute <mm> millimetre : LengthUnit;
    attribute <cm> centimetre : LengthUnit;
    attribute <km> kilometre : LengthUnit;
    attribute <g> gram : MassUnit;
    attribute <mg> milligram : MassUnit;
    attribute <t> tonne : MassUnit;
    attribute <ms> millisecond : DurationUnit;
    attribute <min> minute : DurationUnit;
    attribute <h> hour : DurationUnit;
    attribute <d> day : DurationUnit;
    attribute <mA> milliampere : ElectricCurrentUnit;
    attribute <kHz> kilohertz : FrequencyUnit;
    attribute <MHz> megahertz : FrequencyUnit;
    attribute <GHz> gigahertz : FrequencyUnit;
    attribute <kN> kilonewton : ForceUnit;
    attribute <kPa> kilopascal : PressureUnit;
    attribute <MPa> megapascal : PressureUnit;
    attribute <bar> bar : PressureUnit;
    attribute <kJ> kilojoule : EnergyUnit;
    attribute <MJ> megajoule : EnergyUnit;
    attribute <Wh> 'watt hour' : EnergyUnit;
    attribute <kWh> 'kilowatt hour' : EnergyUnit;
    attribute <mW> milliwatt : PowerUnit;
    attribute <kW> kilowatt : PowerUnit;
    attribute <MW> megawatt : PowerUnit;
    attribute <mV> millivolt : ElectricPotentialDifferenceUnit;
    attribute <kV> kilovolt : ElectricPotentialDifferenceUnit;
    attribute <Ah> 'ampere hour' : ElectricChargeUnit;
    attribute <mAh> 'milliampere hour' : ElectricChargeUnit;
    attribute <L> litre : VolumeUnit;
    attribute <mL> millilitre : VolumeUnit;
    attribute <deg> degree : AngularMeasureUnit;
    attribute <rpm> 'revolution per minute' : AngularVelocityUnit;
}

standard library package USCustomaryUnits {
    doc /* US customary units. */
    private import ISQ::*;
    attribute <ft> foot : LengthUnit;
    attribute <'in'> inch : LengthUnit;
    attribute <mi> mile : LengthUnit;
    attribute <lb> pound : MassUnit;
    attribute <lbf> 'pound force' : ForceUnit;
    attribute <psi> 'pound force per square inch' : PressureUnit;
    attribute <mph> 'mile per hour' : SpeedUnit;
    attribute <gal> gallon : VolumeUnit;
    attribute <hp> horsepower : PowerUnit;
}
