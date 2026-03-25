// @ts-nocheck
// Sample: Jest testing patterns
// Expected detections: describe/it blocks, expect assertions, beforeEach, async tests, mock patterns

interface Calculator {
  add(a: number, b: number): number;
  subtract(a: number, b: number): number;
  multiply(a: number, b: number): number;
  divide(a: number, b: number): number;
}

class SimpleCalculator implements Calculator {
  add(a: number, b: number): number {
    return a + b;
  }

  subtract(a: number, b: number): number {
    return a - b;
  }

  multiply(a: number, b: number): number {
    return a * b;
  }

  divide(a: number, b: number): number {
    if (b === 0) {
      throw new Error("Division by zero");
    }
    return a / b;
  }
}

describe("SimpleCalculator", () => {
  let calculator: SimpleCalculator;

  beforeEach(() => {
    calculator = new SimpleCalculator();
  });

  describe("add", () => {
    it("should add two positive numbers", () => {
      expect(calculator.add(2, 3)).toBe(5);
    });

    it("should add negative numbers", () => {
      expect(calculator.add(-1, -2)).toBe(-3);
    });

    it("should handle zero", () => {
      expect(calculator.add(5, 0)).toBe(5);
    });
  });

  describe("subtract", () => {
    it("should subtract two numbers", () => {
      expect(calculator.subtract(5, 3)).toBe(2);
    });
  });

  describe("multiply", () => {
    it("should multiply two numbers", () => {
      expect(calculator.multiply(3, 4)).toBe(12);
    });

    it("should return zero when multiplied by zero", () => {
      expect(calculator.multiply(5, 0)).toBe(0);
    });
  });

  describe("divide", () => {
    it("should divide two numbers", () => {
      expect(calculator.divide(10, 2)).toBe(5);
    });

    it("should throw on division by zero", () => {
      expect(() => calculator.divide(1, 0)).toThrow("Division by zero");
    });
  });
});

// Async test patterns
describe("async operations", () => {
  async function fetchData(id: string): Promise<{ id: string; value: number }> {
    return { id, value: 42 };
  }

  it("should resolve async data", async () => {
    const result = await fetchData("test-1");
    expect(result.id).toBe("test-1");
    expect(result.value).toBe(42);
  });

  it("should handle async errors", async () => {
    async function failingOp(): Promise<void> {
      throw new Error("async failure");
    }

    await expect(failingOp()).rejects.toThrow("async failure");
  });
});
