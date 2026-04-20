import type {JSONValue} from '../../../shared/src/json.ts';
import type {Storage} from '../../../zql/src/ivm/operator.ts';
import type {Stream} from '../../../zql/src/ivm/stream.ts';
import type {RustTakeState} from '../index.js';

export class RustTakeStorage implements Storage {
  readonly #state: RustTakeState;
  readonly #prefix: string;

  constructor(state: RustTakeState, opID: number) {
    this.#state = state;
    this.#prefix = String(opID);
  }

  get(key: string, def?: JSONValue): JSONValue | undefined {
    const json = this.#state.getState(this.#prefix + ':' + key);
    if (json === null || json === undefined) {
      return def;
    }
    return JSON.parse(json) as JSONValue;
  }

  set(key: string, value: JSONValue): void {
    this.#state.setState(
      this.#prefix + ':' + key,
      JSON.stringify(value),
    );
  }

  del(key: string): void {
    this.#state.delState(this.#prefix + ':' + key);
  }

  *scan(_options?: {prefix: string}): Stream<[string, JSONValue]> {
    throw new Error('RustTakeStorage does not support scan (Take never calls it)');
  }
}
