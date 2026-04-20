import type {JSONValue} from '../../../shared/src/json.ts';
import type {Storage} from '../../../zql/src/ivm/operator.ts';
import type {Stream} from '../../../zql/src/ivm/stream.ts';
import type {RustStorage} from '../index.js';

export class RustTakeStorage implements Storage {
  readonly #store: RustStorage;
  readonly #prefix: string;

  constructor(store: RustStorage, opID: number) {
    this.#store = store;
    this.#prefix = String(opID);
  }

  get(key: string, def?: JSONValue): JSONValue | undefined {
    const json = this.#store.get(this.#prefix + ':' + key);
    if (json === null || json === undefined) {
      return def;
    }
    return JSON.parse(json) as JSONValue;
  }

  set(key: string, value: JSONValue): void {
    this.#store.set(
      this.#prefix + ':' + key,
      JSON.stringify(value),
    );
  }

  del(key: string): void {
    this.#store.del(this.#prefix + ':' + key);
  }

  *scan(options?: {prefix: string}): Stream<[string, JSONValue]> {
    const fullPrefix = this.#prefix + ':' + (options?.prefix ?? '');
    const results = this.#store.scan(fullPrefix);
    for (const pair of results) {
      const rawKey = pair[0];
      // Strip the opID prefix to return the original key
      const key = rawKey.slice(this.#prefix.length + 1);
      yield [key, JSON.parse(pair[1]) as JSONValue];
    }
  }
}
