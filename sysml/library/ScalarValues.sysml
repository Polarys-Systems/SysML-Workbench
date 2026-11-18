standard library package ScalarValues {
    doc /* Primitive data types: truth values, text and numbers. */
    private import Base::*;
    abstract attribute def ScalarValue :> DataValue {
        doc /* A single value that is not made of other values: a truth value, a string or a number. */
    }
    attribute def Boolean :> ScalarValue {
        doc /* A truth value: true or false. Constraints and requirement checks evaluate to Boolean. */
    }
    attribute def String :> ScalarValue {
        doc /* A sequence of characters, written in double quotes: "text". */
    }
    abstract attribute def NumericalValue :> ScalarValue {
        doc /* Any number. The number types nest: every Positive is a Natural, every Natural an Integer, and so on up to Complex. */
    }
    attribute def Complex :> NumericalValue {
        doc /* A complex number. */
    }
    attribute def Real :> Complex {
        doc /* A real number such as 9.81 or 1.5e3. Quantity values (MassValue, LengthValue, ...) are real numbers with a unit. */
    }
    attribute def Rational :> Real {
        doc /* A number that is a ratio of two integers. */
    }
    attribute def Integer :> Rational {
        doc /* A whole number, positive, negative or zero. */
    }
    attribute def Natural :> Integer {
        doc /* A whole number that is zero or more. Multiplicity bounds are naturals. */
    }
    attribute def Positive :> Natural {
        doc /* A whole number that is one or more. */
    }
}

standard library package Quantities {
    doc /* Values of quantities: a number with a measurement reference such as a unit. */
    private import ScalarValues::*;
    abstract attribute def TensorQuantityValue {
        doc /* The most general quantity value: an array of numbers with a measurement reference. */
    }
    abstract attribute def VectorQuantityValue :> TensorQuantityValue {
        doc /* A quantity value made of several numbers, such as a position or a velocity in space. */
    }
    abstract attribute def ScalarQuantityValue :> VectorQuantityValue {
        doc /* A quantity value that is one number with a unit, such as 1200 [kg]. The ISQ value types (MassValue, LengthValue, ...) specialize it. */
    }
    attribute tensorQuantities : TensorQuantityValue [*] nonunique;
    attribute vectorQuantities : VectorQuantityValue [*] nonunique;
    attribute scalarQuantities : ScalarQuantityValue [*] nonunique;
    attribute def DimensionOneValue :> ScalarQuantityValue {
        doc /* A quantity of dimension one (a ratio, a count or an angle), written with unit one or a dimensionless unit such as rad. */
    }
}

standard library package MeasurementReferences {
    doc /* Units and scales that quantity values are expressed in. */
    abstract attribute def MeasurementReference {
        doc /* What the number of a quantity value is measured against: a unit or a scale. */
    }
    abstract attribute def TensorMeasurementReference :> MeasurementReference;
    abstract attribute def ScalarMeasurementReference :> TensorMeasurementReference;
    abstract attribute def MeasurementUnit :> ScalarMeasurementReference {
        doc /* A unit: a quantity value is a number times its unit. */
    }
    attribute def SimpleUnit :> MeasurementUnit {
        doc /* A unit of a base quantity, such as metre or kilogram. */
    }
    attribute def DerivedUnit :> MeasurementUnit {
        doc /* A unit built from base units by products and powers, such as newton (kg*m/s^2). */
    }
    attribute def DimensionOneUnit :> DerivedUnit {
        doc /* A unit of a quantity of dimension one, such as radian. */
    }
    attribute def MeasurementScale :> ScalarMeasurementReference {
        doc /* A scale for quantities that are not a number times a unit. */
    }
    attribute def IntervalScale :> MeasurementScale {
        doc /* A scale whose zero is a convention, such as degree Celsius: differences are meaningful, ratios are not. */
    }
    attribute def OrdinalScale :> MeasurementScale {
        doc /* A scale whose values are only ordered, such as a hardness scale. */
    }
}
