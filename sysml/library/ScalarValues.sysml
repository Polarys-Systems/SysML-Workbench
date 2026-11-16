standard library package ScalarValues {
    doc /* Primitive data types: truth values, text and numbers. */
    private import Base::*;
    abstract attribute def ScalarValue :> DataValue;
    attribute def Boolean :> ScalarValue;
    attribute def String :> ScalarValue;
    abstract attribute def NumericalValue :> ScalarValue;
    attribute def Complex :> NumericalValue;
    attribute def Real :> Complex;
    attribute def Rational :> Real;
    attribute def Integer :> Rational;
    attribute def Natural :> Integer;
    attribute def Positive :> Natural;
}

standard library package Quantities {
    doc /* Values of quantities: a number with a measurement reference such as a unit. */
    private import ScalarValues::*;
    abstract attribute def TensorQuantityValue;
    abstract attribute def VectorQuantityValue :> TensorQuantityValue;
    abstract attribute def ScalarQuantityValue :> VectorQuantityValue;
    attribute tensorQuantities : TensorQuantityValue [*] nonunique;
    attribute vectorQuantities : VectorQuantityValue [*] nonunique;
    attribute scalarQuantities : ScalarQuantityValue [*] nonunique;
    attribute def DimensionOneValue :> ScalarQuantityValue;
}

standard library package MeasurementReferences {
    doc /* Units and scales that quantity values are expressed in. */
    abstract attribute def MeasurementReference;
    abstract attribute def TensorMeasurementReference :> MeasurementReference;
    abstract attribute def ScalarMeasurementReference :> TensorMeasurementReference;
    abstract attribute def MeasurementUnit :> ScalarMeasurementReference;
    attribute def SimpleUnit :> MeasurementUnit;
    attribute def DerivedUnit :> MeasurementUnit;
    attribute def DimensionOneUnit :> DerivedUnit;
    attribute def MeasurementScale :> ScalarMeasurementReference;
    attribute def IntervalScale :> MeasurementScale;
    attribute def OrdinalScale :> MeasurementScale;
}
