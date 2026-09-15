(load (expand-file-name "sexp2json.el" default-directory) nil t)
(dolist (e auto-mode-alist)
  (when (and (stringp (car e)) (symbolp (cdr e)) (cdr e))
    (princ (j (list (car e) (cdr e)))) (terpri)))
