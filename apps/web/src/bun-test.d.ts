declare module "bun:test" {
  interface BunExpectation {
    not: BunExpectation;
    toBe(expected: unknown): void;
    toBeUndefined(): void;
    toEqual(expected: unknown): void;
    toHaveLength(expected: number): void;
    toMatchObject(expected: unknown): void;
  }

  export function describe(name: string, callback: () => void): void;
  export function expect<T>(value: T): BunExpectation;
  export function it(name: string, callback: () => void | Promise<void>): void;
  export function test(name: string, callback: () => void | Promise<void>): void;
}
