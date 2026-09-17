"""Entry module for the pyright half of the raw LSP spike."""

from helper import DEFAULT_LIMIT, Config, Label, choose, identity


# @decl make | make
def make() -> str:
    # @expr Config | Config(DEFAULT_LIMIT)
    config = Config(DEFAULT_LIMIT)
    # @expr doubled | config.doubled()
    count = config.doubled()
    # @expr identity | identity(count)
    count = identity(count)
    # @expr choose | choose(True)
    offset = choose(True)
    # @expr count | count + offset
    total = count + offset
    # @expr label | Label().label()
    label = Label().label()
    # @expr total | f"{label}: {total}"
    return f"{label}: {total}"