#!/bin/bash
R=/tmp/emx/root
export LD_LIBRARY_PATH=$R/usr/lib/x86_64-linux-gnu
export EMACSDATA=$R/usr/share/emacs/29.3/etc
export EMACSLOADPATH=$(find $R/usr/share/emacs/29.3/lisp -type d | tr '\n' ':')
exec $R/usr/bin/emacs-nox --batch "$@"
