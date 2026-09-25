-- declines: not a MIN or a MAX
SELECT SUM(t.production_year) AS total FROM title AS t, movie_keyword AS mk WHERE t.id = mk.movie_id;
