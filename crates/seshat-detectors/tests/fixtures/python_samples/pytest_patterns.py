# Sample: Python pytest testing patterns and conventions
# Expected detections: pytest_tests, fixtures, parametrize, test_classes, raises, markers

from __future__ import annotations

from dataclasses import dataclass
from typing import Generator

import pytest


@dataclass
class Calculator:
    """Simple calculator for testing demonstrations."""

    precision: int = 2

    def add(self, a: float, b: float) -> float:
        """Add two numbers."""
        return round(a + b, self.precision)

    def divide(self, a: float, b: float) -> float:
        """Divide a by b."""
        if b == 0:
            msg = "Cannot divide by zero"
            raise ValueError(msg)
        return round(a / b, self.precision)


@pytest.fixture
def calculator() -> Calculator:
    """Provide a Calculator instance."""
    return Calculator(precision=2)


@pytest.fixture
def sample_data() -> list[float]:
    """Provide sample numeric data."""
    return [1.0, 2.5, 3.7, 4.2, 5.0]


@pytest.fixture
def temp_state() -> Generator[dict[str, object], None, None]:
    """Fixture with setup and teardown."""
    state: dict[str, object] = {"initialized": True}
    yield state
    state.clear()


class TestCalculatorAdd:
    """Tests for Calculator.add method."""

    def test_add_positive(self, calculator: Calculator) -> None:
        """Should add positive numbers."""
        assert calculator.add(2, 3) == 5.0

    def test_add_negative(self, calculator: Calculator) -> None:
        """Should add negative numbers."""
        assert calculator.add(-2, -3) == -5.0

    @pytest.mark.parametrize(
        ("a", "b", "expected"),
        [
            (0, 0, 0.0),
            (1, -1, 0.0),
            (0.1, 0.2, 0.3),
        ],
    )
    def test_add_parametrized(
        self, calculator: Calculator, a: float, b: float, expected: float
    ) -> None:
        """Should handle various input combinations."""
        assert calculator.add(a, b) == expected


class TestCalculatorDivide:
    """Tests for Calculator.divide method."""

    def test_divide_basic(self, calculator: Calculator) -> None:
        """Should divide two numbers."""
        assert calculator.divide(10, 3) == 3.33

    def test_divide_by_zero(self, calculator: Calculator) -> None:
        """Should raise ValueError on division by zero."""
        with pytest.raises(ValueError, match="Cannot divide by zero"):
            calculator.divide(1, 0)


@pytest.mark.slow
def test_with_sample_data(calculator: Calculator, sample_data: list[float]) -> None:
    """Test using multiple fixtures."""
    total = sum(calculator.add(x, 0) for x in sample_data)
    assert total == pytest.approx(16.4, rel=1e-2)


def test_temp_state_cleanup(temp_state: dict[str, object]) -> None:
    """Test that fixture state is accessible."""
    assert temp_state["initialized"] is True
    temp_state["extra"] = "data"
