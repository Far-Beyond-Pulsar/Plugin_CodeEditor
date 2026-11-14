// A typed state machine: string-literal unions, a transition table and a class.

type State = "idle" | "walking" | "jumping";
type Input = "move" | "stop" | "jump" | "land";

/** For each state, the state each input leads to (missing = input ignored). */
const transitions: Record<State, Partial<Record<Input, State>>> = {
  idle: { move: "walking", jump: "jumping" },
  walking: { stop: "idle", jump: "jumping" },
  jumping: { land: "idle" },
};

class StateMachine {
  private state: State = "idle";
  private readonly history: State[] = [];

  get current(): State {
    return this.state;
  }

  /** Applies an input; returns true when the state changed. */
  send(input: Input): boolean {
    const next = transitions[this.state][input];
    if (next === undefined) {
      return false;
    }
    this.history.push(this.state);
    this.state = next;
    return true;
  }

  /** The states visited before the current one, oldest first. */
  get visited(): readonly State[] {
    return this.history;
  }
}

const machine = new StateMachine();
for (const input of ["move", "jump", "land", "land"] as Input[]) {
  console.log(`${input}: ${machine.send(input)} -> ${machine.current}`);
}
