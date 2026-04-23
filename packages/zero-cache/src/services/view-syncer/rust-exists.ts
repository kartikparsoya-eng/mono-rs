import type {rustExistsPushBatch as RustExistsPushBatchFn} from '../../../../zero-ivm-rs/index.js';
import type {CompoundKey} from '../../../../zero-protocol/src/ast.ts';
import {ChangeIndex} from '../../../../zql/src/ivm/change-index.ts';
import {ChangeType} from '../../../../zql/src/ivm/change-type.ts';
import {
  makeAddChange,
  makeRemoveChange,
  type Change,
} from '../../../../zql/src/ivm/change.ts';
import type {Node} from '../../../../zql/src/ivm/data.ts';
import type {
  FilterOperator,
  FilterOutput,
} from '../../../../zql/src/ivm/filter-operators.ts';
import type {SourceSchema} from '../../../../zql/src/ivm/schema.ts';
import type {Stream} from '../../../../zql/src/ivm/stream.ts';

type RustBindings = {
  rustExistsPushBatch: typeof RustExistsPushBatchFn;
};

let rustBindings: RustBindings | undefined;
try {
  rustBindings = require('zero-ivm-rs');
} catch {
  rustBindings = undefined;
}

export function isRustExistsAvailable(): boolean {
  return (
    rustBindings !== undefined &&
    typeof rustBindings.rustExistsPushBatch === 'function'
  );
}

type ExistsAction = {
  action: string;
  exists: boolean | null;
};

function* fetchSize(
  node: Node,
  relationshipName: string,
): Generator<'yield', number> {
  const relationship = node.relationships[relationshipName];
  if (!relationship) return 0;
  let size = 0;
  for (const n of relationship()) {
    if (n === 'yield') {
      yield 'yield';
    } else {
      size++;
    }
  }
  return size;
}

export function createRustExistsWrapper(
  original: FilterOperator,
  relationshipName: string,
  _parentJoinKey: CompoundKey,
  existsType: 'EXISTS' | 'NOT EXISTS',
): FilterOperator {
  const notExists = existsType === 'NOT EXISTS';
  let capturedOutput: FilterOutput | undefined;

  const wrapper: FilterOperator = {
    setFilterOutput(output: FilterOutput): void {
      capturedOutput = output;
      original.setFilterOutput(output);
    },

    beginFilter(): void {
      original.beginFilter();
    },

    endFilter(): void {
      original.endFilter();
    },

    *filter(node: Node): Generator<'yield', boolean> {
      return yield* original.filter(node);
    },

    destroy(): void {
      original.destroy();
    },

    getSchema(): SourceSchema {
      return original.getSchema();
    },

    *push(change: Change): Stream<'yield'> {
      if (!rustBindings || !capturedOutput) {
        yield* original.push(change, wrapper);
        return;
      }

      const changeType = change[ChangeIndex.TYPE];

      if (
        changeType === ChangeType.ADD ||
        changeType === ChangeType.EDIT ||
        changeType === ChangeType.REMOVE
      ) {
        yield* original.push(change, wrapper);
        return;
      }

      if (changeType !== ChangeType.CHILD) {
        yield* original.push(change, wrapper);
        return;
      }

      const childData = change[ChangeIndex.CHILD_DATA];
      const childRelName = childData.relationshipName;
      const childChange = childData.change;
      const childType = childChange[ChangeIndex.TYPE];

      if (
        childRelName !== relationshipName ||
        childType === ChangeType.EDIT ||
        childType === ChangeType.CHILD
      ) {
        yield* original.push(change, wrapper);
        return;
      }

      const node = change[ChangeIndex.NODE];
      const size = yield* fetchSize(node, relationshipName);

      const childTypeStr = childType === ChangeType.ADD ? 'add' : 'remove';
      const input = [
        {
          type: 'child',
          childRelationship: childRelName,
          childChangeType: childTypeStr,
          fetchedSize: size,
        },
      ];

      const resultJson = rustBindings.rustExistsPushBatch(
        JSON.stringify(input),
        relationshipName,
        notExists,
      );
      const actions: ExistsAction[] = JSON.parse(resultJson);
      const action = actions[0];

      if (!action) {
        yield* original.push(change, wrapper);
        return;
      }

      switch (action.action) {
        case 'convert_add':
          yield* capturedOutput.push(makeAddChange(node), wrapper);
          return;

        case 'convert_remove':
          if (notExists) {
            yield* capturedOutput.push(
              makeRemoveChange({
                row: node.row,
                relationships: {
                  ...node.relationships,
                  [relationshipName]: () => [],
                },
              }),
              wrapper,
            );
          } else {
            yield* capturedOutput.push(
              makeRemoveChange({
                row: node.row,
                relationships: {
                  ...node.relationships,
                  [relationshipName]: () => [childChange[ChangeIndex.NODE]],
                },
              }),
              wrapper,
            );
          }
          return;

        case 'pass_filter_with_exists': {
          const exists = action.exists === true;
          const passes = notExists ? !exists : exists;
          if (passes) {
            yield* capturedOutput.push(change, wrapper);
          }
          return;
        }

        default:
          // eslint-disable-next-line no-console
          console.warn(
            `[rust-exists] Unknown action '${action.action}', falling back to TS`,
          );
          yield* original.push(change, wrapper);
          return;
      }
    },
  };

  return wrapper;
}
