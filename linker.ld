PHDRS
{
  text PT_LOAD FLAGS(5);   /* R_E */
  data PT_LOAD FLAGS(6);   /* RW_ */
  probes PT_NOTE FLAGS(4); /* R__ */
}

SECTIONS {
    .probes : {
        KEEP(*(.probes))
    } :data
}

/* Ensure we don't interfere with the default script */
SECTIONS {
  .text : { *(.text*) } :text
  .rodata : { *(.rodata*) } :text
  .data.rel.ro : { *(.data.rel.ro*) } :data
  .data : { *(.data*) } :data
  .bss : { *(.bss*) } :data
}