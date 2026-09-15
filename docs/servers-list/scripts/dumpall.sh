#!/bin/bash
# $1 = output dir, rest = files ; heads fixed
out=$1; shift
for f in "$@"; do echo "$f"; done | xargs -P 8 -I{} sh -c './emacs.sh -l sexp2json.el "{}" make-lsp-client make-lsp--client defcustom defvar lsp-dependency defconst defun lsp-defun cl-defun defvar-local > "'$out'/$(basename {}).jsonl" 2>/dev/null'
